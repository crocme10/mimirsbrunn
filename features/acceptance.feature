@regression @acceptance
Feature: Non-Regression
  List of scenarios for geocoding

  Background:
    Given admins have been loaded using cosmogony from Ile de France
    Given streets have been loaded using bano from Ile de France
    Given addresses have been loaded using osm from Ile de France

  @fullSpell @street
  Scenario Outline: Simple street search
    When the user searches for "<query>"
    Then he finds "<id>" in the first <limit> results.

    Examples:
      | query             | id             | limit       |
      | rue hector malot  | id3234         | 3           |


  Scenario Outline: Incomplete search
    When the user searches for "<query>"
    Then he finds "<id>" in the first <limit> results.

    Examples:
      | query             | id             | limit       |
      | rue hect          | id3234         | 3           |
      | rue hect mal      | id3234         | 3           |


  Scenario Outline: Search using abbreviations
    When the user searches for "<query>"
    Then he finds "<id>" in the first <limit> results.

    Examples:
      | query             | id             | limit       |
      | bd diderot        |                | 3           |
      | av beaumarchais   |                | 3           |


  Scenario Outline: Search using elision
    When the user searches for "<query>"
    Then he finds "<id>" in the first <limit> results.

    Examples:
      | query             | id             | limit       |
      | gare d'austerlitz |                | 3           |
      | gare dausterlitz  |                | 3           |
      | gare d austerlitz |                | 3           |
      | gare austerlitz   |                | 3           |
      | av jeanne d'arc   |                | 3           |
      | av jeanne d arc   |                | 3           |
      | av jeanne darc    |                | 3           |
      | av jeanne arc     |                | 3           |