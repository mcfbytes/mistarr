CREATE VIEW title_search_source AS
  SELECT t.id, t.base_name,
         char(31) || (SELECT p.slug FROM platforms p WHERE p.id = t.platform) || char(31) AS platform
  FROM titles t;
