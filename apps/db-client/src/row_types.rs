//! The types the catalogue's rows are items of, declared beside the resource
//! they are rows of: a row says what it is — its type's noun and the app's
//! name — so search and an agent name it as they name a resource.

pub(crate) use db_client_model::rows::{
    Column, Database, Job, QueryHistory, QueryHistoryEntry, QueryResult, Schema, Table,
    Transaction, View,
};
