use async_trait::async_trait;
use serde::de::DeserializeOwned;

use crate::domain::{
    ports::{
        query::Query,
        search::{Error as PortError, Search, SearchParameters as PrimaryParameters},
    },
    usecases::{Error as UseCaseError, UseCase},
};

pub struct SearchDocuments<D> {
    pub query: Box<dyn Query<Doc = D> + Send + Sync + 'static>,
}

impl<D> SearchDocuments<D> {
    pub fn new(query: Box<dyn Query<Doc = D> + Send + Sync + 'static>) -> Self {
        SearchDocuments { query }
    }
}

pub struct SearchDocumentsParameters {
    pub parameters: PrimaryParameters,
}

#[async_trait]
impl<D: DeserializeOwned + Send + Sync + 'static> UseCase for SearchDocuments<D> {
    type Res = Vec<D>;
    type Param = SearchDocumentsParameters;

    async fn execute(&self, param: Self::Param) -> Result<Self::Res, UseCaseError> {
        self.search_documents(param.parameters)
            .await
            .map_err(|err| UseCaseError::Execution {
                source: Box::new(err),
            })
    }
}

#[async_trait]
impl<D: DeserializeOwned + Send + Sync + 'static> Search for SearchDocuments<D> {
    type Doc = D;
    async fn search_documents(
        &self,
        parameters: PrimaryParameters,
    ) -> Result<Vec<Self::Doc>, PortError> {
        let parameters = crate::domain::ports::query::SearchParameters::from(parameters);
        self.query
            .search_documents(parameters)
            .await
            .map_err(|err| PortError::DocumentRetrievalError {
                source: Box::new(err),
            })
    }
}
