//! Real end-to-end round-trips against live `PostgreSQL` and `MySQL` servers,
//! driven through the production [`Session`] path (`add_connection` opens the
//! real engine, `run_query` runs real SQL, and the schema is introspected from
//! the live catalog).
//!
//! These require reachable servers and are explicitly ignored by ordinary test
//! runs. `tests/run-live.sh` starts disposable rootless-Podman `PostgreSQL` and
//! `MariaDB` containers and selects the ignored tests. Everything the driver
//! layer does with no server (connection-param building, row/column mapping)
//! is proven unconditionally in the crate's unit tests.
//!
//! Connection overrides (all optional):
//!   Postgres: `TILO_DB_TEST_PG_HOST` (default 127.0.0.1),
//!             `_PORT` (5432), `_USER` (postgres), `_PASS` (""), `_DB` (postgres)
//!   `MySQL`:    `TILO_DB_TEST_MYSQL_HOST` (127.0.0.1),
//!             `_PORT` (3306), `_USER` (root), `_PASS` (""), `_DB` (mysql)

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use db_client::engine::MAX_QUERY_ROWS;
use db_client::model::{Cell, ConnectionProfile, Engine};
use db_client::session::Session;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// A table name unique to this run so parallel/repeat runs never collide.
fn unique_table() -> String {
    format!("tilo_dbtest_{}", uuid::Uuid::new_v4().simple())
}

fn postgres_profile(name: &str) -> ConnectionProfile {
    let mut profile = ConnectionProfile::new(name, Engine::Postgres);
    profile.host = env_or("TILO_DB_TEST_PG_HOST", "127.0.0.1");
    profile.port = env_or("TILO_DB_TEST_PG_PORT", "5432").parse().unwrap();
    profile.user = env_or("TILO_DB_TEST_PG_USER", "postgres");
    profile.password = env_or("TILO_DB_TEST_PG_PASS", "");
    profile.database = env_or("TILO_DB_TEST_PG_DB", "postgres");
    profile
}

fn mysql_profile(name: &str) -> ConnectionProfile {
    let mut profile = ConnectionProfile::new(name, Engine::MySql);
    profile.host = env_or("TILO_DB_TEST_MYSQL_HOST", "127.0.0.1");
    profile.port = env_or("TILO_DB_TEST_MYSQL_PORT", "3306").parse().unwrap();
    profile.user = env_or("TILO_DB_TEST_MYSQL_USER", "root");
    profile.password = env_or("TILO_DB_TEST_MYSQL_PASS", "");
    profile.database = env_or("TILO_DB_TEST_MYSQL_DB", "mysql");
    profile
}

/// A real `PostgreSQL` round-trip through the production session, and cancellation stops the active generation.
#[allow(
    clippy::too_many_lines,
    reason = "the live fixture intentionally keeps setup, value assertions, bounds, and cleanup in one transaction-shaped proof"
)]
#[test]
#[ignore = "requires PostgreSQL; run apps/db-client/tests/run-live.sh"]
fn postgres_real_round_trip_and_cancellation() {
    // --- phase: postgres_real_round_trip ---
    {
        let mut s = Session::new();
        s.add_connection_blocking(postgres_profile("live-pg"));
        assert!(s.connected, "postgres connect failed: {:?}", s.status);

        let table = unique_table();
        s.run_query(&format!(
            "CREATE TABLE {table} (\
            id integer PRIMARY KEY, name text NOT NULL, amount numeric, uid uuid, day date, \
            ts timestamp, tz timestamptz, doc jsonb, nullable text\
        )"
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
            "INSERT INTO {table} (id, name, amount, uid, day, ts, tz, doc, nullable) VALUES (\
            1, 'alice', 1.23, '550e8400-e29b-41d4-a716-446655440000', DATE '2024-01-02', \
            TIMESTAMP '2024-01-02 03:04:05', TIMESTAMPTZ '2024-01-02 03:04:05+00', \
            '{{\"n\":7,\"ok\":true}}'::jsonb, NULL\
        )"
        ));
        s.block_until_idle();
        let ins = s.query_result.as_ref().expect("insert result");
        assert!(!ins.is_select, "INSERT should be DML");
        assert_eq!(ins.rows_affected, Some(1), "one row inserted");

        s.run_query(&format!(
            "SELECT id, name, amount, uid, day, ts, tz, doc, nullable FROM {table} ORDER BY id"
        ));
        s.block_until_idle();
        let sel = s.query_result.as_ref().expect("select result");
        assert!(sel.is_select);
        assert_eq!(sel.rows.len(), 1, "one real row returned");
        assert_eq!(sel.rows[0][0].display(), "1");
        assert_eq!(sel.rows[0][1].display(), "alice");
        assert_eq!(sel.rows[0][2].display(), "1.23");
        assert_eq!(
            sel.rows[0][3].display(),
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(sel.rows[0][4].display(), "2024-01-02");
        assert_eq!(sel.rows[0][5].display(), "2024-01-02 03:04:05");
        assert_eq!(sel.rows[0][6].display(), "2024-01-02T03:04:05+00:00");
        assert_eq!(sel.rows[0][7].display(), r#"{"n":7,"ok":true}"#);
        assert_eq!(sel.rows[0][8], Cell::Null);

        s.run_query(
            "SELECT \
            ARRAY[1, NULL, -2]::int4[] AS ints, \
            ARRAY['alpha', NULL, 'two words']::text[] AS labels, \
            ARRAY[DATE '2024-01-02', NULL]::date[] AS days, \
            ARRAY['{\"x\":1}'::jsonb, NULL]::jsonb[] AS docs, \
            ARRAY[]::uuid[] AS empty_ids",
        );
        s.block_until_idle();
        let arrays = s.query_result.as_ref().expect("array select result");
        assert_eq!(arrays.rows.len(), 1);
        assert_eq!(arrays.rows[0][0].display(), "[1, NULL, -2]");
        assert_eq!(
            arrays.rows[0][1].display(),
            r#"["alpha", NULL, "two words"]"#
        );
        assert_eq!(arrays.rows[0][2].display(), r#"["2024-01-02", NULL]"#);
        assert_eq!(arrays.rows[0][3].display(), r#"[{"x":1}, NULL]"#);
        assert_eq!(arrays.rows[0][4].display(), "[]");

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
        assert_eq!(t.row_count, Some(1), "row count from the live catalog");

        // The streaming PostgreSQL path must retain at most the visible result
        // limit, while consuming one sentinel row to honestly mark truncation.
        s.run_query(&format!(
            "SELECT generate_series(1, {}) AS n",
            MAX_QUERY_ROWS + 2
        ));
        s.block_until_idle();
        let bounded = s.query_result.as_ref().expect("bounded select result");
        assert_eq!(bounded.rows.len(), MAX_QUERY_ROWS);
        assert!(bounded.truncated, "the sentinel row must mark truncation");
        assert_eq!(bounded.rows[0][0].display(), "1");
        assert_eq!(
            bounded.rows[MAX_QUERY_ROWS - 1][0].display(),
            MAX_QUERY_ROWS.to_string()
        );

        // Clean up so repeat runs against a persistent server stay green.
        s.run_query(&format!("DROP TABLE {table}"));
        s.block_until_idle();
        assert!(matches!(&s.status, db_client::session::RunStatus::Ok(_)));
    }

    // --- phase: postgres_real_cancellation_stops_the_active_generation ---
    {
        let mut s = Session::new();
        s.add_connection_blocking(postgres_profile("live-pg-cancel"));
        assert!(s.connected, "postgres connect failed: {:?}", s.status);
        s.run_query("SELECT pg_sleep(30)");
        tilo_test_env::wait_until("the worker to publish the active PostgreSQL query", || {
            s.cancel_in_flight_query()
        });
        let cancelled = ui::time::now();
        s.block_until_idle();
        let stopped = cancelled.elapsed();
        assert!(
            stopped < std::time::Duration::from_secs(10),
            "cancelling took {stopped:?}; the 30 s PostgreSQL sleep was not stopped"
        );
        assert!(!s.busy());
        assert!(matches!(s.status, db_client::session::RunStatus::Idle));
    }
}

/// A real `MariaDB` round-trip through the production session, and cancellation stops the active generation.
#[test]
#[ignore = "requires MariaDB; run apps/db-client/tests/run-live.sh"]
fn mysql_real_round_trip_and_cancellation() {
    // --- phase: mysql_real_round_trip ---
    {
        let mut s = Session::new();
        s.add_connection_blocking(mysql_profile("live-mysql"));
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

    // --- phase: mysql_real_cancellation_stops_the_active_generation ---
    {
        let mut s = Session::new();
        s.add_connection_blocking(mysql_profile("live-mysql-cancel"));
        assert!(s.connected, "mysql connect failed: {:?}", s.status);
        s.run_query("SELECT SLEEP(30)");
        tilo_test_env::wait_until("the worker to publish the active MySQL query", || {
            s.cancel_in_flight_query()
        });
        let cancelled = ui::time::now();
        s.block_until_idle();
        let stopped = cancelled.elapsed();
        assert!(
            stopped < std::time::Duration::from_secs(10),
            "cancelling took {stopped:?}; the 30 s MySQL sleep was not stopped"
        );
        assert!(!s.busy());
        assert!(matches!(s.status, db_client::session::RunStatus::Idle));
    }
}
