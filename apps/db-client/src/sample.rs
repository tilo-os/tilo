//! The bundled sample database — a compact music-catalog schema in the
//! spirit of the well-known "chinook" example. It ships with the app so the
//! sidebar, SQL editor and results table all work with zero external
//! setup, both in the live window and in headless `--shots` rendering.
//!
//! The schema deliberately exercises every feature of the UI: primary keys,
//! foreign keys, secondary indexes, `NULLable` columns, a view, and a mix of
//! integer / text / real / datetime types so the type icons and NULL styling
//! have something to show.

use rusqlite::Connection;

/// Create the sample schema and populate it. Idempotent enough for an
/// in-memory handle (it assumes a fresh, empty database).
///
/// # Errors
/// Returns the underlying `SQLite` error if the schema or seed data batch
/// fails to execute.
pub fn seed(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_SQL)?;
    conn.execute_batch(DATA_SQL)?;
    Ok(())
}

const SCHEMA_SQL: &str = r"
CREATE TABLE artists (
    artist_id   INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    country     TEXT,
    founded     INTEGER
);

CREATE TABLE albums (
    album_id    INTEGER PRIMARY KEY,
    title       TEXT NOT NULL,
    artist_id   INTEGER NOT NULL REFERENCES artists(artist_id),
    release_date DATE,
    rating      REAL
);

CREATE TABLE genres (
    genre_id    INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE
);

CREATE TABLE tracks (
    track_id    INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    album_id    INTEGER REFERENCES albums(album_id),
    genre_id    INTEGER REFERENCES genres(genre_id),
    duration_ms INTEGER NOT NULL,
    unit_price  NUMERIC(10,2) NOT NULL DEFAULT 0.99,
    explicit    BOOLEAN NOT NULL DEFAULT 0,
    composer    TEXT
);

CREATE TABLE customers (
    customer_id INTEGER PRIMARY KEY,
    first_name  TEXT NOT NULL,
    last_name   TEXT NOT NULL,
    email       TEXT NOT NULL UNIQUE,
    city        TEXT,
    country     TEXT,
    signup_at   DATETIME
);

CREATE TABLE invoices (
    invoice_id  INTEGER PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers(customer_id),
    invoice_date DATETIME NOT NULL,
    total       NUMERIC(10,2) NOT NULL
);

CREATE INDEX idx_albums_artist ON albums(artist_id);
CREATE INDEX idx_tracks_album ON tracks(album_id);
CREATE INDEX idx_tracks_genre ON tracks(genre_id);
CREATE INDEX idx_invoices_customer ON invoices(customer_id);
CREATE INDEX idx_customers_country ON customers(country);

CREATE VIEW track_catalog AS
SELECT t.track_id,
       t.name        AS track,
       ar.name       AS artist,
       al.title      AS album,
       g.name        AS genre,
       t.unit_price
FROM tracks t
LEFT JOIN albums al ON al.album_id = t.album_id
LEFT JOIN artists ar ON ar.artist_id = al.artist_id
LEFT JOIN genres g ON g.genre_id = t.genre_id;
";

const DATA_SQL: &str = r"
INSERT INTO genres (genre_id, name) VALUES
 (1,'Rock'),(2,'Jazz'),(3,'Electronic'),(4,'Classical'),(5,'Hip-Hop'),(6,'Ambient');

INSERT INTO artists (artist_id, name, country, founded) VALUES
 (1,'Aurora Lights','SE',2011),
 (2,'The Meridian','GB',1998),
 (3,'Kojo Mensah','GH',2015),
 (4,'Vesper Quartet','AT',1987),
 (5,'Nova Drift',NULL,2019),
 (6,'Lena Ostrova','RU',2008),
 (7,'Brass & Co','US',1976);

INSERT INTO albums (album_id, title, artist_id, release_date, rating) VALUES
 (1,'Northern Skies',1,'2014-05-02',4.6),
 (2,'Glass Horizon',2,'2001-11-20',4.1),
 (3,'Highlife Forever',3,'2017-03-10',4.8),
 (4,'Strings at Dusk',4,'1995-09-01',4.9),
 (5,'Signal Bloom',5,'2020-07-15',3.9),
 (6,'Cold Static',5,'2022-02-28',NULL),
 (7,'Velvet Hours',6,'2010-12-05',4.3),
 (8,'Big Band Revival',7,'1981-06-14',4.0);

INSERT INTO tracks (track_id, name, album_id, genre_id, duration_ms, unit_price, explicit, composer) VALUES
 (1,'First Light',1,6,254000,0.99,0,'A. Lindqvist'),
 (2,'Polar Drift',1,3,301500,0.99,0,'A. Lindqvist'),
 (3,'Borealis',1,6,420000,1.29,0,NULL),
 (4,'Meridian Line',2,1,198000,0.99,0,'T. Hale'),
 (5,'Glasswork',2,1,233000,0.99,1,'T. Hale'),
 (6,'Accra Nights',3,5,210000,0.99,0,'K. Mensah'),
 (7,'Highlife Forever',3,5,265000,1.29,0,'K. Mensah'),
 (8,'Adagio in G',4,4,512000,1.49,0,'F. Brunner'),
 (9,'Nocturne',4,4,388000,1.49,0,'F. Brunner'),
 (10,'Bloom',5,3,276000,0.99,0,NULL),
 (11,'Decay',6,3,244000,0.99,1,NULL),
 (12,'White Noise',6,6,600000,0.79,0,NULL),
 (13,'Velvet Hours',7,2,329000,1.29,0,'L. Ostrova'),
 (14,'Midnight Rail',7,2,287000,1.29,0,'L. Ostrova'),
 (15,'Swing Street',8,2,201000,0.99,0,'M. Carter');

INSERT INTO customers (customer_id, first_name, last_name, email, city, country, signup_at) VALUES
 (1,'Mara','Wojcik','mara.w@example.com','Krakow','PL','2023-01-12 09:30:00'),
 (2,'Diego','Santos','diego.s@example.com','Lisbon','PT','2023-02-03 14:05:00'),
 (3,'Aiko','Tanaka','aiko.t@example.com','Osaka','JP','2023-02-19 22:11:00'),
 (4,'Noah','Bergman','noah.b@example.com',NULL,'SE','2023-03-30 08:45:00'),
 (5,'Priya','Nair','priya.n@example.com','Kochi','IN','2023-04-22 17:20:00'),
 (6,'Tom','Becker','tom.b@example.com','Berlin','DE',NULL);

INSERT INTO invoices (invoice_id, customer_id, invoice_date, total) VALUES
 (1,1,'2023-05-01 10:00:00',3.96),
 (2,1,'2023-06-14 11:30:00',1.29),
 (3,2,'2023-05-21 09:15:00',2.58),
 (4,3,'2023-07-02 20:00:00',4.47),
 (5,4,'2023-07-19 12:40:00',0.79),
 (6,5,'2023-08-05 16:05:00',2.28),
 (7,2,'2023-08-30 13:00:00',1.49);
";

/// A few starter queries surfaced in the editor's "snippets" menu so a
/// first-time user can run something interesting immediately.
pub fn starter_queries() -> &'static [(&'static str, &'static str)] {
    &[
        (
            "Top tracks by price",
            "SELECT name, unit_price, duration_ms\nFROM tracks\nORDER BY unit_price DESC, name\nLIMIT 20;",
        ),
        (
            "Albums with artist",
            "SELECT al.title AS album, ar.name AS artist, al.rating\nFROM albums al\nJOIN artists ar ON ar.artist_id = al.artist_id\nORDER BY al.rating DESC;",
        ),
        (
            "Revenue by customer",
            "SELECT c.first_name || ' ' || c.last_name AS customer,\n       COUNT(i.invoice_id) AS invoices,\n       ROUND(SUM(i.total), 2) AS spent\nFROM customers c\nLEFT JOIN invoices i ON i.customer_id = c.customer_id\nGROUP BY c.customer_id\nORDER BY spent DESC;",
        ),
        (
            "Catalog view",
            "SELECT track, artist, album, genre, unit_price\nFROM track_catalog\nORDER BY artist, album\nLIMIT 50;",
        ),
        (
            "Track listing",
            "SELECT t.name AS track, ar.name AS artist,\n       al.title AS album, t.unit_price AS price,\n       g.name AS genre\nFROM tracks t\nJOIN albums al ON al.album_id = t.album_id\nJOIN artists ar ON ar.artist_id = al.artist_id\nJOIN genres g ON g.genre_id = t.genre_id\nORDER BY ar.name, al.title, t.track_id;",
        ),
        (
            "Tracks per genre",
            "SELECT g.name AS genre, COUNT(*) AS tracks\nFROM tracks t\nJOIN genres g ON g.genre_id = t.genre_id\nGROUP BY g.genre_id\nORDER BY tracks DESC;",
        ),
    ]
}

/// The query the editor opens with on first launch.
pub const DEFAULT_QUERY: &str = "SELECT al.title AS album, ar.name AS artist, al.rating, al.release_date\nFROM albums al\nJOIN artists ar ON ar.artist_id = al.artist_id\nORDER BY al.rating DESC;";
