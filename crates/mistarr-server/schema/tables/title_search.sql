CREATE VIRTUAL TABLE title_search USING fts5(
  base_name, platform, content = 'title_search_source', content_rowid = 'id', tokenize = 'trigram'
);
