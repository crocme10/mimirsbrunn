use elasticsearch::cat::CatIndicesParts;
use elasticsearch::http::response::Exception;
use elasticsearch::indices::{
    IndicesCreateParts, IndicesDeleteAliasParts, IndicesDeleteParts, IndicesGetAliasParts,
    IndicesPutAliasParts, IndicesRefreshParts,
};
use elasticsearch::ingest::IngestPutPipelineParts;
use elasticsearch::{BulkOperation, BulkParts, Elasticsearch};
use futures::stream::{Stream, StreamExt};
use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::{ResultExt, Snafu};
use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::sync::{Arc, Mutex};

use super::ElasticsearchStorage;
use crate::domain::model::configuration::{self, Configuration};
use crate::domain::model::index::{Index, IndexStatus};

static CHUNK_SIZE: usize = 10;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(display("Invalid Index Configuration: {}", details))]
    InvalidConfiguration { details: String },

    /// Elasticsearch Error
    #[snafu(display("Elasticsearch Error: {} [{}]", source, details))]
    ElasticsearchError {
        details: String,
        source: elasticsearch::Error,
    },

    /// Elasticsearch Not Created
    #[snafu(display("Elasticsearch Response: Not Created: {}", details))]
    NotCreated { details: String },

    /// Elasticsearch Not Deleted
    #[snafu(display("Elasticsearch Response: Not Deleted: {}", details))]
    NotDeleted { details: String },

    /// Elasticsearch Not Acknowledged
    #[snafu(display("Elasticsearch Response: Not Acknowledged: {}", details))]
    NotAcknowledged { details: String },

    /// Elasticsearch Document Insertion Exception
    #[snafu(display("Elasticsearch Failure without Exception: {}", details))]
    ElasticsearchFailureWithoutException { details: String },

    /// Elasticsearch Unhandled Exception
    #[snafu(display("Elasticsearch Unhandled Exception: {}", details))]
    ElasticsearchUnhandledException { details: String },

    /// Elasticsearch Duplicate Index
    #[snafu(display("Elasticsearch Duplicate Index: {}", index))]
    ElasticsearchDuplicateIndex { index: String },

    /// Elasticsearch Failed To Parse
    #[snafu(display("Elasticsearch Failed to Parse"))]
    ElasticsearchFailedToParse,

    /// Elasticsearch Unknown Index
    #[snafu(display("Elasticsearch Unknown Index: {}", index))]
    ElasticsearchUnknownIndex { index: String },

    /// Elasticsearch Unknown Setting
    #[snafu(display("Elasticsearch Unknown Setting: {}", setting))]
    ElasticsearchUnknownSetting { setting: String },

    /// Elasticsearch Index Conversion
    #[snafu(display("Index Conversion Error: {}", details))]
    IndexConversion { details: String },

    /// Elasticsearch Deserialization Error
    #[snafu(display("JSON Elasticsearch Deserialization Error: {}", source))]
    JsonDeserializationError { source: elasticsearch::Error },

    /// Elasticsearch Deserialization Error
    #[snafu(display("JSON Serde Deserialization Error: {}", source))]
    Json2DeserializationError {
        source: serde_json::Error,
        details: String,
    },

    /// Invalid JSON Value
    #[snafu(display("JSON Deserialization Invalid: {}", details))]
    JsonDeserializationInvalid { details: String },
}

/// The indices create index API has 4 components, which are
/// reproduced below:
/// - Path parameter: The index name
/// - Query parameters: Things like timeout, wait for active shards, ...
/// - Request body, including
///   - Aliases (not implemented here)
///   - Mappings
///   - Settings
///   See https://www.elastic.co/guide/en/elasticsearch/reference/7.12/indices-create-index.html
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfiguration {
    pub name: String,
    pub parameters: IndexParameters,
    pub settings: IndexSettings,
    pub mappings: IndexMappings,
}

// FIXME A lot of work needs to go in there to type everything
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexSettings {
    pub value: String,
}

// FIXME A lot of work needs to go in there to type everything
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexMappings {
    pub value: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename = "snake_case")]
pub struct IndexParameters {
    pub timeout: String,
    pub wait_for_active_shards: String,
}

impl TryFrom<Configuration> for IndexConfiguration {
    type Error = Error;

    // FIXME Parameters not handled
    fn try_from(configuration: Configuration) -> Result<Self, Self::Error> {
        let Configuration { value, .. } = configuration;
        serde_json::from_str(&value).map_err(|err| Error::InvalidConfiguration {
            details: format!(
                "could not deserialize index configuration: {} / {}",
                err.to_string(),
                value
            ),
        })
    }
}

impl From<Exception> for Error {
    // This function analyzes the content of an elasticsearch exception,
    // and returns an error, the type of which should mirror the exception's content.
    // There is no clear blueprint for this analysis, it's very much adhoc.
    fn from(exception: Exception) -> Error {
        let root_cause = exception.error().root_cause();
        if root_cause.is_empty() {
            // TODO If we can't find a root cause, not sure how to handle that.
            Error::ElasticsearchUnhandledException {
                details: String::from("Unspecified root cause"),
            }
        } else {
            lazy_static! {
                static ref ALREADY_EXISTS: Regex =
                    Regex::new(r"index \[([^\]/]+).*\] already exists").unwrap();
            }
            lazy_static! {
                static ref NOT_FOUND: Regex = Regex::new(r"no such index \[([^\]/]+).*\]").unwrap();
            }
            lazy_static! {
                static ref FAILED_PARSE: Regex = Regex::new(r"failed to parse").unwrap();
            }
            lazy_static! {
                static ref UNKNOWN_SETTING: Regex =
                    Regex::new(r"unknown setting \[([^\]/]+).*\]").unwrap();
            }
            match root_cause[0].reason() {
                Some(reason) => {
                    if let Some(caps) = ALREADY_EXISTS.captures(reason) {
                        let index = String::from(caps.get(1).unwrap().as_str());
                        Error::ElasticsearchDuplicateIndex { index }
                    } else if let Some(caps) = NOT_FOUND.captures(reason) {
                        let index = String::from(caps.get(1).unwrap().as_str());
                        Error::ElasticsearchUnknownIndex { index }
                    } else if FAILED_PARSE.is_match(reason) {
                        Error::ElasticsearchFailedToParse
                    } else if let Some(caps) = UNKNOWN_SETTING.captures(reason) {
                        let setting = String::from(caps.get(1).unwrap().as_str());
                        Error::ElasticsearchUnknownSetting { setting }
                    } else {
                        Error::ElasticsearchUnhandledException {
                            details: format!("Unidentified reason: {}", reason),
                        }
                    }
                }
                None => Error::ElasticsearchUnhandledException {
                    details: String::from("Unspecified reason"),
                },
            }
        }
    }
}

impl ElasticsearchStorage {
    pub fn new(client: Elasticsearch) -> ElasticsearchStorage {
        ElasticsearchStorage(client)
    }

    pub(super) async fn create_index(&self, config: IndexConfiguration) -> Result<(), Error> {
        let body_str = format!(
            r#"{{ "mappings": {mappings}, "settings": {settings} }}"#,
            mappings = config.mappings.value,
            settings = config.settings.value
        );
        let body: serde_json::Value =
            serde_json::from_str(&body_str).context(Json2DeserializationError {
                details: String::from("could not deserialize index configuration"),
            })?;

        let response = self
            .0
            .indices()
            .create(IndicesCreateParts::Index(&config.name))
            .timeout(&config.parameters.timeout)
            .wait_for_active_shards(&config.parameters.wait_for_active_shards)
            .body(body)
            .send()
            .await
            .context(ElasticsearchError {
                details: format!("cannot index document '{}'", config.name),
            })?;

        if response.status_code().is_success() {
            // Response similar to:
            // Object({"acknowledged": Bool(true), "index": String("name"), "shards_acknowledged": Bool(true)})
            // We verify that acknowledge is true, then add the cat indices API to get the full index.
            let json = response
                .json::<Value>()
                .await
                .context(JsonDeserializationError)?;

            let acknowledged = json
                .as_object()
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected JSON object"),
                })?
                .get("acknowledged")
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected 'acknowledged'"),
                })?
                .as_bool()
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected JSON bool"),
                })?;
            if acknowledged {
                Ok(())
            } else {
                Err(Error::NotCreated {
                    details: format!("index creation {}", config.name),
                })
            }
        } else {
            let exception = response.exception().await.ok().unwrap();
            match exception {
                Some(exception) => {
                    let err = Error::from(exception);
                    Err(err)
                }
                None => Err(Error::ElasticsearchFailureWithoutException {
                    details: String::from("Fail status without exception"),
                }),
            }
        }
    }

    pub(super) async fn delete_index(&self, index: String) -> Result<(), Error> {
        let response = self
            .0
            .indices()
            .delete(IndicesDeleteParts::Index(&[&index]))
            .send()
            .await
            .context(ElasticsearchError {
                details: format!("cannot find index '{}'", index),
            })?;

        if response.status_code().is_success() {
            // Response similar to:
            // Object({"acknowledged": Bool(true), "index": String("name"), "shards_acknowledged": Bool(true)})
            // We verify that acknowledge is true, then add the cat indices API to get the full index.
            let json = response
                .json::<Value>()
                .await
                .context(JsonDeserializationError)?;

            let acknowledged = json
                .as_object()
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected JSON object"),
                })?
                .get("acknowledged")
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected 'acknowledged'"),
                })?
                .as_bool()
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected JSON bool"),
                })?;

            if acknowledged {
                Ok(())
            } else {
                Err(Error::NotDeleted {
                    details: String::from(
                        "Elasticsearch response to index deletion not acknowledged",
                    ),
                })
            }
        } else {
            let exception = response.exception().await.ok().unwrap();
            match exception {
                Some(exception) => {
                    let err = Error::from(exception);
                    Err(err)
                }
                None => Err(Error::ElasticsearchFailureWithoutException {
                    details: String::from("Fail status without exception"),
                }),
            }
        }
    }

    // FIXME Move details to impl ElasticsearchStorage.
    pub(super) async fn find_index(&self, index: String) -> Result<Option<Index>, Error> {
        let response = self
            .0
            .cat()
            .indices(CatIndicesParts::Index(&[&index]))
            .format("json")
            .send()
            .await
            .context(ElasticsearchError {
                details: format!("cannot find index '{}'", index),
            })?;

        if response.status_code().is_success() {
            let json = response
                .json::<Value>()
                .await
                .context(JsonDeserializationError)?;

            let mut indices: Vec<ElasticsearchIndex> =
                serde_json::from_value(json).context(Json2DeserializationError {
                    details: String::from("could not deserialize Elasticsearch indices"),
                })?;

            indices.pop().map(Index::try_from).transpose()
        } else {
            let exception = response.exception().await.ok().unwrap();

            match exception {
                Some(exception) => {
                    let err = Error::from(exception);
                    Err(err)
                }
                None => Err(Error::ElasticsearchFailureWithoutException {
                    details: String::from("Fail status without exception"),
                }),
            }
        }
    }

    /* Commented out because it is not used
    pub(super) async fn insert_document<D>(
        &self,
        index: String,
        id: String,
        document: D,
    ) -> Result<(), Error>
    where
        D: Serialize + Send + Sync + 'static,
    {
        let response = self
            .0
            .index(IndexParts::IndexId(&index, &id))
            .body(document)
            .send()
            .await
            .context(ElasticsearchError {
                details: format!("cannot index document '{}'", id),
            })?;

        if response.status_code().is_success() {
            // Response similar to:
            // {
            //   "_id": "AvypLXkBazLmtM_qtw9a",
            //   "_index": "munin_book_books_20210502_151927_673737330",
            //   "_primary_term": 1, "_seq_no": 0,
            //   "_shards": {
            //     "failed": 0, "successful": 1, "total": 2
            //   },
            //   "_type": "_doc",
            //   "_version": 1,
            //   "result": "created"
            // }
            // We verify that result is "created"
            let json = response
                .json::<Value>()
                .await
                .context(JsonDeserializationError)?;

            let result = json
                .as_object()
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected JSON object"),
                })?
                .get("result")
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected 'result'"),
                })?
                .as_str()
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected JSON string"),
                })?;
            if result == "created" {
                Ok(())
            } else {
                Err(Error::NotCreated {
                    details: format!("document creation {}", id),
                })
            }
        } else {
            let exception = response.exception().await.ok().unwrap();

            match exception {
                Some(exception) => {
                    let err = Error::from(exception);
                    Err(err)
                }
                None => Err(Error::ElasticsearchFailureWithoutException {
                    details: String::from("Fail status without exception"),
                }),
            }
        }
    }
    */

    // Changed the name to avoid recursive calls int storage::insert_documents
    pub(super) async fn insert_documents_in_index<S, D>(
        &self,
        index: String,
        documents: S,
    ) -> Result<usize, Error>
    where
        D: Serialize + Send + Sync + 'static,
        S: Stream<Item = D> + Send + Sync + Unpin + 'static,
    {
        let counter_created = Arc::new(Mutex::new(0_usize));

        documents
            .chunks(CHUNK_SIZE) // FIXME chunck size should be a variable.
            .for_each(|chunk| {
                let counter_created = counter_created.clone();
                let index = index.clone();
                async move {
                    let mut ops: Vec<BulkOperation<Value>> = Vec::with_capacity(CHUNK_SIZE);
                    chunk.iter().for_each(|doc| {
                        let value = serde_json::to_value(doc).expect("to json value");
                        ops.push(BulkOperation::index(value).into());
                    });
                    // FIXME Missing Error Handling
                    let resp = self
                        .0
                        .bulk(BulkParts::Index(index.as_str()))
                        .body(ops)
                        .send()
                        .await
                        .expect("send bulk");

                    if resp.status_code().is_success() {
                        if let Ok(json) = resp.json::<Value>().await {
                            let items = json.as_object().expect("response is a json object")
                                ["items"]
                                .as_array()
                                .expect("Array of items");
                            items.iter().for_each(|item| {
                                let result = item.as_object().expect("item is a json object")
                                    ["index"]
                                    .as_object()
                                    .expect("index object")["result"]
                                    .as_str()
                                    .expect("result is a string");
                                if result == "created" {
                                    let mut count_guard = (*counter_created).lock().unwrap();
                                    *count_guard += 1;
                                }
                            })
                        }
                    } else {
                        let exception = resp.exception().await.unwrap();

                        match exception {
                            Some(exception) => {
                                let err = Error::from(exception);
                                println!("es exception: {}", err);
                            }
                            None => {
                                println!("no exception in bulk insertion");
                            }
                        }
                    }
                }
            })
            .await;
        let count = *counter_created.lock().unwrap();
        Ok(count)
    }

    pub(super) async fn add_alias(&self, indices: Vec<String>, alias: String) -> Result<(), Error> {
        let indices = indices.iter().map(String::as_str).collect::<Vec<_>>();
        let response = self
            .0
            .indices()
            .put_alias(IndicesPutAliasParts::IndexName(&indices, &alias))
            .send()
            .await
            .context(ElasticsearchError {
                details: format!(
                    "cannot add alias '{}' to indices '{}'",
                    alias,
                    indices.join(" ")
                ),
            })?;

        if response.status_code().is_success() {
            // Response similar to:
            // Object({"acknowledged": Bool(true)})
            // We verify that acknowledge is true, then add the cat indices API to get the full index.
            let json = response
                .json::<Value>()
                .await
                .context(JsonDeserializationError)?;

            let acknowledged = json
                .as_object()
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected JSON object"),
                })?
                .get("acknowledged")
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected 'acknowledged'"),
                })?
                .as_bool()
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected JSON boolean"),
                })?;

            if acknowledged {
                Ok(())
            } else {
                Err(Error::NotAcknowledged {
                    details: format!("alias {} creation", alias),
                })
            }
        } else {
            let exception = response.exception().await.ok().unwrap();

            match exception {
                Some(exception) => {
                    let err = Error::from(exception);
                    Err(err)
                }
                None => Err(Error::ElasticsearchFailureWithoutException {
                    details: String::from("Fail status without exception"),
                }),
            }
        }
    }

    pub(super) async fn remove_alias(
        &self,
        indices: Vec<String>,
        alias: String,
    ) -> Result<(), Error> {
        let indices = indices.iter().map(String::as_str).collect::<Vec<_>>();
        let response = self
            .0
            .indices()
            .delete_alias(IndicesDeleteAliasParts::IndexName(&indices, &[&alias]))
            .send()
            .await
            .context(ElasticsearchError {
                details: format!(
                    "cannot remove alias '{}' to indices '{}'",
                    alias,
                    indices.join(" ")
                ),
            })?;

        if response.status_code().is_success() {
            // Response similar to:
            // Object({"acknowledged": Bool(true)})
            // We verify that acknowledge is true, then add the cat indices API to get the full index.
            let json = response
                .json::<Value>()
                .await
                .context(JsonDeserializationError)?;

            let acknowledged = json
                .as_object()
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected JSON object"),
                })?
                .get("acknowledged")
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected 'acknowledged'"),
                })?
                .as_bool()
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected JSON boolean"),
                })?;

            if acknowledged {
                Ok(())
            } else {
                Err(Error::NotAcknowledged {
                    details: format!("alias {} deletion", alias),
                })
            }
        } else {
            let exception = response.exception().await.ok().unwrap();

            match exception {
                Some(exception) => {
                    let err = Error::from(exception);
                    Err(err)
                }
                None => Err(Error::ElasticsearchFailureWithoutException {
                    details: String::from("Fail status without exception"),
                }),
            }
        }
    }

    pub(super) async fn find_aliases(
        &self,
        index: String,
    ) -> Result<BTreeMap<String, Vec<String>>, Error> {
        // The last piece of the input index should be a dataset
        // If you didn't add the trailing '_*' below, when you would search for
        // the aliases of eg 'fr', you would also find the aliases for 'fr-ne'.
        let index = format!("{}_*", index);
        let response = self
            .0
            .indices()
            .get_alias(IndicesGetAliasParts::Index(&[&index]))
            .send()
            .await
            .context(ElasticsearchError {
                details: format!("cannot find aliases to {}", index),
            })?;

        if response.status_code().is_success() {
            // Response similar to:
            // {
            //   "index1": {
            //      "aliases": {
            //         "alias1": {},
            //         "alias2": {}
            //      }
            //   },
            //   "index2": {
            //      "aliases": {
            //         "alias3": {}
            //      }
            //   }
            // }
            let json = response
                .json::<Value>()
                .await
                .context(JsonDeserializationError)?;

            let obj = json.as_object().ok_or(Error::JsonDeserializationInvalid {
                details: String::from("expected JSON object"),
            })?;

            let mut aliases = BTreeMap::new();
            for (key, value) in obj {
                let x = value.as_object().expect("aliases object")["aliases"]
                    .as_object()
                    .expect("list of aliases");
                let y = x.keys().map(|key| String::from(key)).collect::<Vec<_>>();
                aliases.insert(String::from(key), y); // should not be worrying about duplicate entries ??
            }
            Ok(aliases)
        } else {
            let exception = response.exception().await.ok().unwrap();

            match exception {
                Some(exception) => {
                    let err = Error::from(exception);
                    Err(err)
                }
                None => Err(Error::ElasticsearchFailureWithoutException {
                    details: String::from("Fail status without exception"),
                }),
            }
        }
    }

    pub(super) async fn add_pipeline(&self, pipeline: String, name: String) -> Result<(), Error> {
        let pipeline: serde_json::Value =
            serde_json::from_str(&pipeline).context(Json2DeserializationError {
                details: format!("Could not deserialize pipeline {}", name),
            })?;
        let response = self
            .0
            .ingest()
            .put_pipeline(IngestPutPipelineParts::Id(&name))
            .body(pipeline)
            .send()
            .await
            .context(ElasticsearchError {
                details: format!("cannot add pipeline '{}'", name,),
            })?;

        if response.status_code().is_success() {
            // Response similar to:
            // Object({"acknowledged": Bool(true)})
            // We verify that acknowledge is true, then add the cat indices API to get the full index.
            let json = response
                .json::<Value>()
                .await
                .context(JsonDeserializationError)?;

            let acknowledged = json
                .as_object()
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected JSON object"),
                })?
                .get("acknowledged")
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected 'acknowledged'"),
                })?
                .as_bool()
                .ok_or(Error::JsonDeserializationInvalid {
                    details: String::from("expected JSON boolean"),
                })?;

            if acknowledged {
                Ok(())
            } else {
                Err(Error::NotAcknowledged {
                    details: format!("pipeline {} creation", name),
                })
            }
        } else {
            let exception = response.exception().await.ok().unwrap();

            match exception {
                Some(exception) => {
                    let err = Error::from(exception);
                    Err(err)
                }
                None => Err(Error::ElasticsearchFailureWithoutException {
                    details: String::from("Fail status without exception"),
                }),
            }
        }
    }

    pub(super) async fn get_previous_indices(&self, index: &Index) -> Result<Vec<String>, Error> {
        let base_index = configuration::root_doctype_dataset(&index.doc_type, &index.dataset);
        // FIXME When available, we can use aliases.into_keys
        let aliases = self.find_aliases(base_index).await?;
        Ok(aliases
            .into_iter()
            .map(|(k, _)| k)
            .filter(|i| i.as_str() != index.name)
            .collect())
    }

    pub(super) async fn refresh_index(&self, index: String) -> Result<(), Error> {
        let response = self
            .0
            .indices()
            .refresh(IndicesRefreshParts::Index(&[&index]))
            .send()
            .await
            .context(ElasticsearchError {
                details: format!("cannot refresh index {}", index),
            })?;

        // Note We won't analyze the details of the response.
        if !response.status_code().is_success() {
            let exception = response.exception().await.ok().unwrap();

            match exception {
                Some(exception) => {
                    let err = Error::from(exception);
                    Err(err)
                }
                None => Err(Error::ElasticsearchFailureWithoutException {
                    details: String::from("Fail status without exception"),
                }),
            }
        } else {
            Ok(())
        }
    }

    // This implementation uses the scroll API, which is not recommended anymore for deep
    // pagination.
    pub(super) async fn retrieve_all_documents<S, D>(&self, _index: String) -> Result<S, Error>
    where
        D: Serialize + Send + Sync + 'static,
        S: Stream<Item = D> + Send + Sync + Unpin + 'static,
    {
        unimplemented!();
    }
}

/// This is the information provided by Elasticsearch CAT Indice API
#[derive(PartialEq, Debug, Serialize, Deserialize)]
pub struct ElasticsearchIndex {
    pub health: String,
    pub status: String,
    #[serde(rename = "index")]
    pub name: String,
    #[serde(rename = "docs.count")]
    pub docs_count: Option<String>,
    #[serde(rename = "docs.deleted")]
    pub docs_deleted: Option<String>,
    pub pri: String,
    #[serde(rename = "pri.store.size")]
    pub pri_store_size: Option<String>,
    pub rep: String,
    #[serde(rename = "store.size")]
    pub store_size: Option<String>,
    pub uuid: String,
}

impl TryFrom<ElasticsearchIndex> for Index {
    type Error = Error;
    fn try_from(index: ElasticsearchIndex) -> Result<Self, Self::Error> {
        let ElasticsearchIndex {
            name,
            docs_count,
            status,
            ..
        } = index;
        let (doc_type, dataset) =
            configuration::split_index_name(&name).map_err(|err| Error::IndexConversion {
                details: format!(
                    "could not convert elasticsearch index into model index: {}",
                    err.to_string()
                ),
            })?;

        let docs_count = match docs_count {
            Some(val) => val.parse::<u32>().expect("docs count"),
            None => 0,
        };
        Ok(Index {
            name,
            doc_type,
            dataset,
            docs_count,
            status: IndexStatus::from(status),
        })
    }
}

impl From<String> for IndexStatus {
    fn from(status: String) -> Self {
        match status.as_str() {
            "green" => IndexStatus::Available,
            "yellow" => IndexStatus::Available,
            _ => IndexStatus::Available,
        }
    }
}

/*
#[derive(Deserialize)]
struct ItemResponse {
    items: Vec<Items>,
    scroll_id: Option<String>,
}

enum State {
    Start(Option<String>),
    Next(String),
    End,
}


pub(super) async fn retrieve_all_documents<S, D>(&self, index: String, after: Option<String>) -> Result<S, Error>
    where
        D: Serialize + Send + Sync + 'static,
        S: Stream<Item = D> + Send + Sync + Unpin + 'static,
{
    // (definition of State enum can go here)

    let s = stream::unfold(
        State::Start(after), move |state| {
            let scroll_id = match state {
                State::Start(opt_ct) => opt_ct,
                State::Next(ct) => Some(ct),
                State::End => return None,
            };
            let scroll = match scroll_id {
                Some(ct) => self
                    .0
                    .scroll(ScrollParts::None)
                    .body(serde_json::json!({
                        "scroll": scroll,
                        "scroll_id": scroll_id
                    })),
                None => self
                .0
                .search(SearchParts::Index(&[index]))
                .scroll(scroll)
                .body(serde_json::json!({
                    "query": {
                        "match_all": { }
                    }
                }))
            };

            let response = scroll.send().await?
                .context(ElasticsearchError {
                    details: format!("cannot refresh index {}", index),
                })?;

            let response_body = response.json::<Value>().await?;
            let scroll_id = response_body["_scroll_id"].as_str().unwrap();
            let hits = response_body["hits"]["hits"].as_array().unwrap();

        let req = Request::new(Method::Get, url.parse().unwrap());
        Some(client.request(req).from_err().and_then(move |resp| {
            let status = resp.status();
            resp.body().concat2().from_err().and_then(move |body| {
                if status.is_success() {
                    serde_json::from_slice::<ItemsResponse>(&body)
                        .map_err(Box::<Error>::from)
                } else {
                    Err(format!("HTTP status: {}", status).into())
                }
            })
            .map(move |items_resp| {
                let next_state = match items_resp.continuation_token {
                    Some(ct) => State::Next(ct),
                    None => State::End,
                };
                (stream::iter_ok(items_resp.items), next_state)
            })
        }))
    })
    .flatten())
}
let scroll = "1m"; // How long Elasticsearch should retain the search context for the request.
        let mut response = self
            .0
            .search(SearchParts::Index(&[index]))
            .scroll(scroll)
            .body(serde_json::json!({
                "query": {
                    "match_all": { }
                }
            }))
            .send()
            .await
            .context(ElasticsearchError {
                details: format!("cannot refresh index {}", index),
            })?;

        if !response.status_code().is_success() {
            let exception = response.exception().await.ok().unwrap();

            match exception {
                Some(exception) => {
                    let err = Error::from(exception);
                    return Err(err);
                }
                None => return Err(Error::ElasticsearchFailureWithoutException {
                    details: String::from("Fail status without exception"),
                });
            }
        }

        let json = response
            .json::<Value>()
            .await
            .context(JsonDeserializationError)?;

        let scroll_id = json
            .as_object()
            .ok_or(Error::JsonDeserializationInvalid {
                details: String::from("expected JSON object"),
            })?
            .get("_scroll_id")
            .ok_or(Error::JsonDeserializationInvalid {
                details: String::from("expected '_scroll_id'"),
            })?
            .as_str()
            .ok_or(Error::JsonDeserializationInvalid {
                details: String::from("expected JSON string"),
            })?;

        let hits = json
            .as_object()
            .ok_or(Error::JsonDeserializationInvalid {
                details: String::from("expected JSON object"),
            })?
            .get("hits")
            .ok_or(Error::JsonDeserializationInvalid {
                details: String::from("expected 'hits'"),
            })?
            .as_object()
            .ok_or(Error::JsonDeserializationInvalid {
                details: String::from("expected JSON object"),
            })?
            .get("hits")
            .ok_or(Error::JsonDeserializationInvalid {
                details: String::from("expected 'hits'"),
            })?
            .as_array()
            .ok_or(Error::JsonDeserializationInvalid {
                details: String::from("expected JSON array"),
            })?;

        while hits.len() > 0 {
            response = self
                .0
                .scroll(ScrollParts::None)
                .body(serde_json::json!({
                    "scroll": scroll,
                    "scroll_id": scroll_id
                }))
                .send()
                .await?
                .context(ElasticsearchError {
                    details: format!("cannot refresh index {}", index),
                })?;

            response_body = response.json::<Value>().await?;
            scroll_id = response_body["_scroll_id"].as_str().unwrap();
            hits = response_body["hits"]["hits"].as_array().unwrap();
            print_hits(hits);
        }

        response = client
            .clear_scroll(ClearScrollParts::None)
            .body(json!({ "scroll_id": scroll_id }))
            .send()
            .await?;
    }

*/