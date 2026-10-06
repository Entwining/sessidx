#[cfg(test)]
mod tests {
    #[test]
    fn bundled_fts5_cjk_phrase_and_latin_word() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE VIRTUAL TABLE fts USING fts5(text, tokenize='unicode61'); INSERT INTO fts VALUES ('你 这 条 太 长 了 Latin words');").unwrap();
        for query in ["\"太 长\"", "Latin"] {
            let count: i64 = db
                .query_row("SELECT count(*) FROM fts WHERE fts MATCH ?", [query], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(count, 1);
        }
        let count: i64 = db
            .query_row("SELECT count(*) FROM fts WHERE fts MATCH 'Lat'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
}
pub mod adapters;
pub mod discovery;
pub mod model;
pub mod normalize;
pub mod query;
pub mod redaction;
pub mod store;
