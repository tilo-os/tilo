//! Real end-to-end round-trips against live `PostgreSQL` and `MySQL` servers,
//! driven through the production [`Session`] path (`add_connection` opens the
//! real engine, `run_query` runs real SQL, and the schema is introspected from
//! the live catalog).
//!
//! These require a reachable server, so each is gated behind an env var — set
//! it (and the connection env vars) to run the round-trip; unset, the test
//! prints why it is inert and returns. The CI/dev proof drives them against
//! rootless-podman `postgres:16` / `mariadb` containers. Everything the driver
//! layer does with no server (connection-param building, row/column mapping)
//! is proven unconditionally in the crate's unit tests.
//!
//! Env vars (all optional except the gate):
//!   Postgres: `QOS_DB_TEST_PG` (gate), `QOS_DB_TEST_PG_HOST` (def 127.0.0.1),
//!             `_PORT` (5432), `_USER` (postgres), `_PASS` (""), `_DB` (postgres)
//!   `MySQL`:    `QOS_DB_TEST_MYSQL` (gate), `QOS_DB_TEST_MYSQL_HOST` (127.0.0.1),
//!             `_PORT` (3306), `_USER` (root), `_PASS` (""), `_DB` (mysql)

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use db_client::model::{ConnectionProfile, Engine};
use db_client::session::Session;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// A table name unique to this run so parallel/repeat runs never collide.
fn unique_table() -> String {
    format!("qos_dbtest_{}", uuid::Uuid::new_v4().simple())
}

#[test]
fn postgres_real_round_trip() {
    if std::env::var("QOS_DB_TEST_PG").is_err() {
        eprintln!(
            "skipping postgres_real_round_trip: set QOS_DB_TEST_PG to run against a live server"
        );
        return;
    }

    let mut profile = ConnectionProfile::new("live-pg", Engine::Postgres);
    profile.host = env_or("QOS_DB_TEST_PG_HOST", "127.0.0.1");
    profile.port = env_or("QOS_DB_TEST_PG_PORT", "5432").parse().unwrap();
    profile.user = env_or("QOS_DB_TEST_PG_USER", "postgres");
    profile.password = env_or("QOS_DB_TEST_PG_PASS", "");
    profile.database = env_or("QOS_DB_TEST_PG_DB", "postgres");

    let mut s = Session::new();
    s.add_connection_blocking(profile);
    assert!(s.connected, "postgres connect failed: {:?}", s.status);

    let table = unique_table();
    s.run_query(&format!(
        "CREATE TABLE {table} (id integer PRIMARY KEY, name text NOT NULL)"
    ));
    s.block_until_idle();
    let create = s.query_result.as_ref();
    assert!(
        matches!(&s.status, db_client::session::RunStatus::Ok(_)),
        "CREATE failed: {:?}",
        s.status
    );
    // DDL is non-select.
    assert!(create.is_none_or(|r| !r.is_select));

    s.run_query(&format!(
        "INSERT INTO {table} (id, name) VALUES (1, 'alice'), (2, 'bob')"
    ));
    s.block_until_idle();
    let ins = s.query_result.as_ref().expect("insert result");
    assert!(!ins.is_select, "INSERT should be DML");
    assert_eq!(ins.rows_affected, Some(2), "two rows inserted");

    s.run_query(&format!("SELECT id, name FROM {table} ORDER BY id"));
    s.block_until_idle();
    let sel = s.query_result.as_ref().expect("select result");
    assert!(sel.is_select);
    assert_eq!(sel.columns, vec!["id".to_string(), "name".to_string()]);
    assert_eq!(sel.rows.len(), 2, "two real rows returned");
    assert_eq!(sel.rows[0][0].display(), "1");
    assert_eq!(sel.rows[0][1].display(), "alice");
    assert_eq!(sel.rows[1][1].display(), "bob");

    // The re-introspected live catalog now carries the new table with its PK
    // and a real row count.
    let t = s
        .schema
        .find_table(&table)
        .unwrap_or_else(|| panic!("introspected schema is missing {table}"));
    assert!(
        t.columns.iter().any(|c| c.name == "id" && c.primary_key),
        "id must be discovered as a primary key"
    );
    assert!(
        t.columns.iter().any(|c| c.name == "name"),
        "name column must be discovered"
    );
    assert_eq!(t.row_count, Some(2), "row count from the live catalog");

    // Clean up so repeat runs against a persistent server stay green.
    s.run_query(&format!("DROP TABLE {table}"));
    s.block_until_idle();
    assert!(matches!(&s.status, db_client::session::RunStatus::Ok(_)));
}

#[test]
fn mysql_real_round_trip() {
    if std::env::var("QOS_DB_TEST_MYSQL").is_err() {
        eprintln!(
            "skipping mysql_real_round_trip: set QOS_DB_TEST_MYSQL to run against a live server"
        );
        return;
    }

    let mut profile = ConnectionProfile::new("live-mysql", Engine::MySql);
    profile.host = env_or("QOS_DB_TEST_MYSQL_HOST", "127.0.0.1");
    profile.port = env_or("QOS_DB_TEST_MYSQL_PORT", "3306").parse().unwrap();
    profile.user = env_or("QOS_DB_TEST_MYSQL_USER", "root");
    profile.password = env_or("QOS_DB_TEST_MYSQL_PASS", "");
    profile.database = env_or("QOS_DB_TEST_MYSQL_DB", "mysql");

    let mut s = Session::new();
    s.add_connection_blocking(profile);
    assert!(s.connected, "mysql connect failed: {:?}", s.status);

    let table = unique_table();
    s.run_query(&format!(
        "CREATE TABLE {table} (id INT PRIMARY KEY, name VARCHAR(64) NOT NULL)"
    ));
    s.block_until_idle();
    assert!(
        matches!(&s.status, db_client::session::RunStatus::Ok(_)),
        "CREATE failed: {:?}",
        s.status
    );

    s.run_query(&format!(
        "INSERT INTO {table} (id, name) VALUES (1, 'alice'), (2, 'bob')"
    ));
    s.block_until_idle();
    let ins = s.query_result.as_ref().expect("insert result");
    assert!(!ins.is_select, "INSERT should be DML");
    assert_eq!(ins.rows_affected, Some(2), "two rows inserted");

    s.run_query(&format!("SELECT id, name FROM {table} ORDER BY id"));
    s.block_until_idle();
    let sel = s.query_result.as_ref().expect("select result");
    assert!(sel.is_select);
    assert_eq!(sel.columns, vec!["id".to_string(), "name".to_string()]);
    assert_eq!(sel.rows.len(), 2, "two real rows returned");
    assert_eq!(sel.rows[0][0].display(), "1");
    assert_eq!(sel.rows[0][1].display(), "alice");
    assert_eq!(sel.rows[1][1].display(), "bob");
    // MySQL integers map to a typed integer cell (not text).
    assert!(sel.rows[0][0].is_numeric(), "id is a typed integer cell");

    let t = s
        .schema
        .find_table(&table)
        .unwrap_or_else(|| panic!("introspected schema is missing {table}"));
    assert!(
        t.columns.iter().any(|c| c.name == "id" && c.primary_key),
        "id must be discovered as a primary key"
    );
    assert_eq!(t.row_count, Some(2), "row count from the live catalog");

    s.run_query(&format!("DROP TABLE {table}"));
    s.block_until_idle();
    assert!(matches!(&s.status, db_client::session::RunStatus::Ok(_)));
}
