//! Tests for the database layer.
//!
//! They live with the module rather than in `tests/` because most of them reach
//! for private helpers: the migration chain, `map_media_row`, and the clustering
//! that `find_duplicates` is built on.

use super::*;

/// The schema version the chain is expected to reach. Update this together with a
/// new migration, which is the point: forgetting makes the test fail loudly here
/// rather than quietly at a user's next startup.
const CURRENT_SCHEMA_VERSION: i32 = 20;

fn migrated() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory database");
    conn.execute("PRAGMA foreign_keys = ON;", []).unwrap();
    Database::migrate(&conn).expect("migration chain should run on an empty database");
    conn
}

fn user_version(conn: &Connection) -> i32 {
    conn.query_row("PRAGMA user_version;", [], |r| r.get(0))
        .unwrap()
}

fn table_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    let names = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    names
}

#[test]
fn migrating_from_empty_reaches_the_current_version() {
    let conn = migrated();
    assert_eq!(user_version(&conn), CURRENT_SCHEMA_VERSION);
}

/// Every block assigns the local `version` after its `PRAGMA user_version`. When
/// they disagree, the chain still happens to work in one direction and breaks the
/// next time a migration is inserted, so assert they agree rather than waiting.
#[test]
fn running_the_chain_twice_is_a_no_op() {
    let conn = migrated();
    let before = table_names(&conn);
    Database::migrate(&conn).expect("re-running the chain should be a no-op");
    assert_eq!(user_version(&conn), CURRENT_SCHEMA_VERSION);
    assert_eq!(before, table_names(&conn));
}

/// The bug this guards against: a migration block sets `PRAGMA user_version` in
/// SQL but forgets to assign the local `version` in Rust. Eight blocks had drifted
/// that way. Nothing breaks immediately, because the blocks run in order and each
/// one only widens the schema, so the omission sits latent until someone inserts a
/// migration whose guard then reads a stale version and skips or repeats work.
///
/// Reading the source is unusual for a test, but the alternative is a runtime
/// assertion that cannot fire until the damage is already possible.
#[test]
fn every_migration_block_updates_the_local_version() {
    let source = include_str!("migrations.rs");
    for n in 1..=CURRENT_SCHEMA_VERSION {
        assert!(
            source.contains(&format!("PRAGMA user_version = {};", n)),
            "migration {} does not set PRAGMA user_version",
            n
        );
        // Matched as a whole line, because `contains` on "version = 12;" also
        // matches the "PRAGMA user_version = 12;" two lines above it, which would
        // make this assertion pass for exactly the code it exists to reject.
        let assignment = format!("version = {};", n);
        assert!(
            source
                .lines()
                .any(|line| line.trim().starts_with(&assignment)),
            "migration {} sets PRAGMA user_version but never assigns the local \
             `version`, so a later migration guard will read a stale value",
            n
        );
    }
}

#[test]
fn the_expected_tables_exist() {
    let conn = migrated();
    let tables = table_names(&conn);
    for required in [
        "media",
        "albums",
        "album_media",
        "upload_queue",
        "tags",
        "media_tags",
        "faces",
        "persons",
        "config",
    ] {
        assert!(
            tables.iter().any(|t| t == required),
            "missing table {}: {:?}",
            required,
            tables
        );
    }
}

/// Migration 15 deletes persons with no faces pointing at them. `NOT IN` against an
/// empty subquery matches every row, so without its guard this wipes every named
/// person on a library where face detection has not run, which is the normal state
/// for most users.
#[test]
fn ghost_person_cleanup_keeps_people_when_no_face_is_assigned() {
    let conn = migrated();
    conn.execute("INSERT INTO persons (name) VALUES ('Ana')", [])
        .unwrap();
    conn.execute("INSERT INTO persons (name) VALUES ('Bo')", [])
        .unwrap();

    // Re-run the cleanup exactly as migration 15 does.
    conn.execute_batch(
        "DELETE FROM persons
         WHERE EXISTS (SELECT 1 FROM faces WHERE person_id IS NOT NULL)
           AND id NOT IN (SELECT person_id FROM faces WHERE person_id IS NOT NULL);",
    )
    .unwrap();

    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM persons", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        remaining, 2,
        "named people were deleted with no faces present"
    );
}

/// The other half of the same guard: once assignments exist, the cleanup still
/// removes the persons that nothing points at.
#[test]
fn ghost_person_cleanup_still_removes_unreferenced_people() {
    let conn = migrated();
    conn.execute(
        "INSERT INTO media (file_path, created_at) VALUES ('/tmp/a.jpg', 0)",
        [],
    )
    .unwrap();
    let media_id: i64 = conn.last_insert_rowid();
    conn.execute("INSERT INTO persons (name) VALUES ('Ana')", [])
        .unwrap();
    let ana: i64 = conn.last_insert_rowid();
    conn.execute("INSERT INTO persons (name) VALUES ('ghost')", [])
        .unwrap();
    conn.execute(
        "INSERT INTO faces (media_id, x, y, width, height, score, person_id) VALUES (?, 0, 0, 1, 1, 1.0, ?)",
        rusqlite::params![media_id, ana],
    )
    .unwrap();

    conn.execute_batch(
        "DELETE FROM persons
         WHERE EXISTS (SELECT 1 FROM faces WHERE person_id IS NOT NULL)
           AND id NOT IN (SELECT person_id FROM faces WHERE person_id IS NOT NULL);",
    )
    .unwrap();

    let names: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM persons ORDER BY name")
            .unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    assert_eq!(names, vec!["Ana".to_string()]);
}

fn insert_media(conn: &Connection, path: &str) -> i64 {
    conn.execute(
        "INSERT INTO media (file_path, created_at) VALUES (?1, 0)",
        [path],
    )
    .unwrap();
    conn.last_insert_rowid()
}

/// The batched writes have to behave exactly like the per-row statements they
/// replace, including the count they report back.
#[test]
fn batched_writes_report_what_they_changed() {
    let temp = TempDb::new();
    let a = insert_media_with_camera(&temp.db, "/library/a.jpg", "Canon");
    let b = insert_media_with_camera(&temp.db, "/library/b.jpg", "Canon");

    assert_eq!(temp.db.update_phashes(&[]).unwrap(), 0);
    let written = temp
        .db
        .update_phashes(&[(a, "aaaa".to_string()), (b, "bbbb".to_string())])
        .unwrap();
    assert_eq!(written, 2);

    let stored: Vec<String> = {
        let conn = temp.db.get_conn().unwrap();
        let mut stmt = conn
            .prepare("SELECT phash FROM media WHERE phash IS NOT NULL ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    assert_eq!(stored, vec!["aaaa".to_string(), "bbbb".to_string()]);

    assert_eq!(temp.db.mark_cloud_only(&[]).unwrap(), 0);
    assert_eq!(temp.db.mark_cloud_only(&[a, b]).unwrap(), 2);
    assert!(temp.db.get_media_by_id(a).unwrap().unwrap().is_cloud_only);
    assert!(temp.db.get_media_by_id(b).unwrap().unwrap().is_cloud_only);
}

/// The manifest export reads album membership in one pass. It has to report every
/// album a photo belongs to, and nothing for a photo in none.
#[test]
fn album_names_are_read_in_one_pass() {
    let temp = TempDb::new();
    let a = insert_media_with_camera(&temp.db, "/library/a.jpg", "Canon");
    let b = insert_media_with_camera(&temp.db, "/library/b.jpg", "Canon");
    let lonely = insert_media_with_camera(&temp.db, "/library/c.jpg", "Canon");

    let holiday = temp.db.create_album("Holiday").unwrap();
    let family = temp.db.create_album("Family").unwrap();
    temp.db.add_media_to_album(holiday, a).unwrap();
    temp.db.add_media_to_album(family, a).unwrap();
    temp.db.add_media_to_album(holiday, b).unwrap();

    let by_media = temp.db.album_names_by_media().unwrap();
    assert_eq!(by_media.len(), 2);

    let mut for_a = by_media.get(&a).cloned().unwrap();
    for_a.sort();
    assert_eq!(for_a, vec!["Family".to_string(), "Holiday".to_string()]);
    assert_eq!(by_media.get(&b).cloned().unwrap(), vec!["Holiday"]);

    // A photo in no album has no entry at all, rather than an empty one, which is
    // what the export's lookup relies on to skip it.
    assert!(!by_media.contains_key(&lonely));
}

/// A `MediaItem` with only the fields the clustering reads set.
fn item_with_id(id: i64) -> MediaItem {
    MediaItem {
        id,
        file_path: format!("/library/{id}.jpg"),
        file_hash: None,
        telegram_media_id: None,
        mime_type: None,
        width: None,
        height: None,
        duration: None,
        size_bytes: None,
        created_at: id,
        uploaded_at: None,
        thumbnail_path: None,
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
        is_favorite: false,
        rating: 0,
        is_deleted: false,
        deleted_at: None,
        is_archived: false,
        archived_at: None,
        is_cloud_only: false,
    }
}

/// The exhaustive scan the bucketing replaces, kept as the oracle.
fn cluster_pairwise(candidates: &[(MediaItem, String)], threshold: u32) -> Vec<Vec<i64>> {
    let n = candidates.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn root(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            x = parent[x];
        }
        x
    }

    for i in 0..n {
        for j in (i + 1)..n {
            let a = ParsedHash::parse(&candidates[i].1);
            let b = ParsedHash::parse(&candidates[j].1);
            let (Some(a), Some(b)) = (a, b) else { continue };
            if a.bits.len() != b.bits.len() {
                continue;
            }
            if a.distance(&b) <= threshold {
                let (ra, rb) = (root(&mut parent, i), root(&mut parent, j));
                if ra != rb {
                    parent[ra] = rb;
                }
            }
        }
    }

    let mut groups: std::collections::HashMap<usize, Vec<i64>> =
        std::collections::HashMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let r = root(&mut parent, index);
        groups.entry(r).or_default().push(candidate.0.id);
    }

    let mut out: Vec<Vec<i64>> = groups
        .into_values()
        .filter(|group| group.len() > 1)
        .collect();
    for group in &mut out {
        group.sort_unstable();
    }
    out.sort();
    out
}

fn hex_hash(value: u64) -> String {
    format!("{value:016x}")
}

/// Bucketing by band is only worth doing if it produces exactly what comparing
/// every pair produced. Generate hashes with a deterministic pseudo-random
/// sequence, seeded so a failure is reproducible, and compare the two.
#[test]
fn bucketed_clustering_matches_the_exhaustive_scan() {
    let mut state = 0x243f_6a88_85a3_08d3u64;
    let mut next = move || {
        // xorshift64: no dependency, and the sequence is fixed.
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let mut candidates: Vec<(MediaItem, String)> = Vec::new();
    let mut id = 1i64;

    for _ in 0..40 {
        let base = next();
        candidates.push((item_with_id(id), hex_hash(base)));
        id += 1;

        // A few near-copies at varying distances, straddling the threshold.
        for flips in [1u32, 4, 9, 10, 11, 20] {
            let mut variant = base;
            for bit in 0..flips {
                variant ^= 1u64 << ((next() as u32 + bit) % 64);
            }
            candidates.push((item_with_id(id), hex_hash(variant)));
            id += 1;
        }
    }

    let expected = cluster_pairwise(&candidates, PHASH_DISTANCE_THRESHOLD);

    let mut actual: Vec<Vec<i64>> = cluster_by_phash(candidates, PHASH_DISTANCE_THRESHOLD)
        .into_iter()
        .map(|group| {
            let mut ids: Vec<i64> = group.into_iter().map(|item| item.id).collect();
            ids.sort_unstable();
            ids
        })
        .collect();
    actual.sort();

    assert_eq!(actual, expected);
    assert!(
        !actual.is_empty(),
        "the fixture should produce at least one duplicate group"
    );
}

/// Groups come back oldest-first inside a group, biggest group first, which is
/// what the review UI relies on to pick a default "keep" item.
#[test]
fn duplicate_groups_keep_their_ordering() {
    let identical = hex_hash(0xdead_beef_dead_beef);
    let other = hex_hash(0x0f0f_0f0f_0f0f_0f0f);

    let candidates = vec![
        (item_with_id(3), identical.clone()),
        (item_with_id(1), identical.clone()),
        (item_with_id(2), identical),
        (item_with_id(5), other.clone()),
        (item_with_id(4), other),
    ];

    let groups = cluster_by_phash(candidates, PHASH_DISTANCE_THRESHOLD);
    assert_eq!(groups.len(), 2);
    assert_eq!(
        groups[0].iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        groups[1].iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![4, 5]
    );
}

/// A hash that parses as neither base64 nor hex must not join a group, and must
/// not take the rest of the library down with it.
#[test]
fn unparseable_hashes_are_left_alone() {
    let candidates = vec![
        (item_with_id(1), hex_hash(0xaaaa_aaaa_aaaa_aaaa)),
        (item_with_id(2), hex_hash(0xaaaa_aaaa_aaaa_aaab)),
        (item_with_id(3), "not a hash".to_string()),
        (item_with_id(4), String::new()),
    ];

    let groups = cluster_by_phash(candidates, PHASH_DISTANCE_THRESHOLD);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0].iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

/// Every read that returns a `MediaItem` goes through `map_media_row`, which
/// addresses columns by index. A query that selects the shared list in another
/// order still compiles and still runs: it just loads the wrong value into each
/// field. Fill one row with a distinct value per column and assert that every
/// entry point returns the same item, so a mismatched `SELECT` fails here.
#[test]
fn every_media_read_maps_the_same_row() {
    let temp = TempDb::new();
    let id = {
        let conn = temp.db.get_conn().unwrap();
        conn.execute(
            "INSERT INTO media (
                 file_path, file_hash, telegram_media_id, mime_type, width, height, duration,
                 size_bytes, created_at, uploaded_at, thumbnail_path, date_taken, latitude,
                 longitude, camera_make, camera_model, is_favorite, rating, is_deleted,
                 deleted_at, is_archived, archived_at, is_cloud_only
             ) VALUES (
                 '/library/one.jpg', 'hash-1', '4242', 'image/jpeg', 640, 480, 12,
                 4096, 1000, 2000, '/library/thumbs/one.jpg', '2024-01-02 03:04:05', 51.5,
                 -0.12, 'Canon', 'EOS R', 1, 5, 0,
                 NULL, 0, NULL, 0
             )",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    };

    let expected = temp.db.get_media_by_id(id).unwrap().expect("row by id");
    assert_eq!(expected.file_path, "/library/one.jpg");
    assert_eq!(expected.file_hash.as_deref(), Some("hash-1"));
    assert_eq!(expected.telegram_media_id.as_deref(), Some("4242"));
    assert_eq!(expected.mime_type.as_deref(), Some("image/jpeg"));
    assert_eq!(expected.width, Some(640));
    assert_eq!(expected.height, Some(480));
    assert_eq!(expected.duration, Some(12));
    assert_eq!(expected.size_bytes, Some(4096));
    assert_eq!(expected.created_at, 1000);
    assert_eq!(expected.uploaded_at, Some(2000));
    assert_eq!(
        expected.thumbnail_path.as_deref(),
        Some("/library/thumbs/one.jpg")
    );
    assert_eq!(expected.date_taken.as_deref(), Some("2024-01-02 03:04:05"));
    assert_eq!(expected.latitude, Some(51.5));
    assert_eq!(expected.longitude, Some(-0.12));
    assert_eq!(expected.camera_make.as_deref(), Some("Canon"));
    assert_eq!(expected.camera_model.as_deref(), Some("EOS R"));
    assert!(expected.is_favorite);
    assert_eq!(expected.rating, 5);
    assert!(!expected.is_deleted);
    assert!(!expected.is_archived);
    assert!(!expected.is_cloud_only);

    let same = |label: &str, item: &MediaItem| {
        assert_eq!(item.id, expected.id, "{label}: id");
        assert_eq!(item.file_path, expected.file_path, "{label}: file_path");
        assert_eq!(item.file_hash, expected.file_hash, "{label}: file_hash");
        assert_eq!(
            item.telegram_media_id, expected.telegram_media_id,
            "{label}: telegram_media_id"
        );
        assert_eq!(item.mime_type, expected.mime_type, "{label}: mime_type");
        assert_eq!(item.width, expected.width, "{label}: width");
        assert_eq!(item.height, expected.height, "{label}: height");
        assert_eq!(item.duration, expected.duration, "{label}: duration");
        assert_eq!(item.size_bytes, expected.size_bytes, "{label}: size_bytes");
        assert_eq!(item.created_at, expected.created_at, "{label}: created_at");
        assert_eq!(
            item.uploaded_at, expected.uploaded_at,
            "{label}: uploaded_at"
        );
        assert_eq!(
            item.thumbnail_path, expected.thumbnail_path,
            "{label}: thumbnail_path"
        );
        assert_eq!(item.date_taken, expected.date_taken, "{label}: date_taken");
        assert_eq!(item.latitude, expected.latitude, "{label}: latitude");
        assert_eq!(item.longitude, expected.longitude, "{label}: longitude");
        assert_eq!(
            item.camera_make, expected.camera_make,
            "{label}: camera_make"
        );
        assert_eq!(
            item.camera_model, expected.camera_model,
            "{label}: camera_model"
        );
        assert_eq!(
            item.is_favorite, expected.is_favorite,
            "{label}: is_favorite"
        );
        assert_eq!(item.rating, expected.rating, "{label}: rating");
        assert_eq!(item.is_cloud_only, expected.is_cloud_only, "{label}: cloud");
    };

    same("get_media", &temp.db.get_media(10, 0).unwrap()[0]);
    same(
        "get_media_by_ids",
        &temp.db.get_media_by_ids(&[id]).unwrap()[0],
    );
    same("get_top_rated", &temp.db.get_top_rated(10, 0).unwrap()[0]);
    same("get_favorites", &temp.db.get_favorites(10, 0).unwrap()[0]);
    same(
        "get_all_media_for_sync",
        &temp.db.get_all_media_for_sync().unwrap()[0],
    );
    same(
        "search_fts",
        &temp
            .db
            .search_fts("", &camera_filter("Canon"), 10, 0)
            .unwrap()[0],
    );
    same(
        "get_next_item_to_scan",
        &temp.db.get_next_item_to_scan().unwrap().expect("pending"),
    );
}

/// Insert a row with a camera, for the filter tests.
fn insert_media_with_camera(db: &Database, path: &str, camera: &str) -> i64 {
    let conn = db.get_conn().unwrap();
    conn.execute(
        "INSERT INTO media (file_path, created_at, camera_make) VALUES (?1, 0, ?2)",
        rusqlite::params![path, camera],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn camera_filter(camera: &str) -> SearchFilters {
    SearchFilters {
        favorites_only: false,
        min_rating: None,
        date_from: None,
        date_to: None,
        camera_make: Some(camera.to_string()),
        has_location: None,
    }
}

#[test]
fn like_escaping_covers_the_three_special_characters() {
    assert_eq!(escape_like_value("Canon"), "Canon");
    assert_eq!(escape_like_value("100%"), "100\\%");
    assert_eq!(escape_like_value("a_b"), "a\\_b");
    // The backslash first, or escaping the others would double-escape it.
    assert_eq!(escape_like_value("a\\b"), "a\\\\b");
}

/// The filter value is user input reaching a `WHERE` clause. It used to be
/// interpolated with doubled quotes; anything the escaping missed was query text.
#[test]
fn a_camera_filter_is_bound_rather_than_interpolated() {
    let temp = TempDb::new();
    insert_media_with_camera(&temp.db, "/library/a.jpg", "Canon");
    insert_media_with_camera(&temp.db, "/library/b.jpg", "Nikon");

    let hostile = "Canon' OR 1=1 --";
    let found = temp
        .db
        .search_fts("", &camera_filter(hostile), 100, 0)
        .unwrap();
    assert!(
        found.is_empty(),
        "a quote in the filter must be a value, not syntax: {:?}",
        found.iter().map(|m| &m.file_path).collect::<Vec<_>>()
    );

    let found = temp
        .db
        .search_fts("", &camera_filter("Canon"), 100, 0)
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].file_path, "/library/a.jpg");
}

/// `%` in a `LIKE` pattern is a wildcard, so an unescaped one silently widens the
/// user's filter instead of matching what they typed.
#[test]
fn a_wildcard_in_a_camera_filter_matches_literally() {
    let temp = TempDb::new();
    insert_media_with_camera(&temp.db, "/library/literal.jpg", "C%N");
    insert_media_with_camera(&temp.db, "/library/canon.jpg", "CANON");

    let found = temp
        .db
        .search_fts("", &camera_filter("C%N"), 100, 0)
        .unwrap();
    assert_eq!(
        found
            .iter()
            .map(|m| m.file_path.as_str())
            .collect::<Vec<_>>(),
        vec!["/library/literal.jpg"]
    );
}

/// Both come straight from the frontend. A negative limit means "no limit" to
/// SQLite, and a negative offset is an error rather than a page.
#[test]
fn paginated_reads_clamp_their_limit_and_offset() {
    let temp = TempDb::new();
    insert_media_with_camera(&temp.db, "/library/a.jpg", "Canon");

    assert!(temp.db.get_media_by_person(1, -1, -5).is_ok());
    assert!(temp.db.get_media_by_tag("holiday", -1, -5).is_ok());
    assert!(temp
        .db
        .search_fts("", &camera_filter("Canon"), -1, -5)
        .is_ok());
}

/// More ids than SQLite will accept as bound variables in one statement.
#[test]
fn reads_and_bulk_writes_chunk_long_id_lists() {
    let temp = TempDb::new();
    let ids: Vec<i64> = (0..(MAX_SQL_VARIABLES * 2 + 1))
        .map(|i| insert_media_with_camera(&temp.db, &format!("/library/{}.jpg", i), "Canon"))
        .collect();

    let found = temp.db.get_media_by_ids(&ids).unwrap();
    assert_eq!(found.len(), ids.len());

    assert_eq!(temp.db.bulk_set_favorite(&ids, true).unwrap(), ids.len());
    assert_eq!(temp.db.bulk_soft_delete(&ids).unwrap(), ids.len());
}

fn fts_matches(conn: &Connection, query: &str) -> Vec<i64> {
    let mut stmt = conn
        .prepare("SELECT rowid FROM media_fts WHERE media_fts MATCH ?1 ORDER BY rowid")
        .unwrap();
    stmt.query_map([query], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
}

/// The whole point of migration 20: the index is no longer something the import
/// path remembers to write. An insert through any code path indexes the row, an
/// update re-indexes it, and a delete removes it.
#[test]
fn the_search_index_is_maintained_by_triggers() {
    let conn = migrated();

    // Inserted with plain SQL, deliberately: no call to `add_media`, because the
    // regression being guarded is precisely rows that arrive by some other path.
    let vacation = insert_media(&conn, "/library/vacation.jpg");
    insert_media(&conn, "/library/invoices.pdf");

    assert_eq!(fts_matches(&conn, "vacation"), vec![vacation]);

    conn.execute(
        "UPDATE media SET file_path = '/library/holiday.jpg' WHERE id = ?1",
        [vacation],
    )
    .unwrap();
    assert!(
        fts_matches(&conn, "vacation").is_empty(),
        "the old term still matches after a rename"
    );
    assert_eq!(fts_matches(&conn, "holiday"), vec![vacation]);

    conn.execute("DELETE FROM media WHERE id = ?1", [vacation])
        .unwrap();
    assert!(
        fts_matches(&conn, "holiday").is_empty(),
        "a deleted row is still in the search index"
    );
    assert_eq!(fts_matches(&conn, "invoices").len(), 1);
}

/// External-content FTS5 stores no copy of the text, so a mismatch between the
/// index and `media` is corruption rather than staleness. `integrity-check`
/// is the only thing that detects it.
#[test]
fn the_search_index_passes_its_own_integrity_check() {
    let conn = migrated();
    insert_media(&conn, "/library/vacation.jpg");
    conn.execute(
        "INSERT INTO media_fts (media_fts) VALUES ('integrity-check')",
        [],
    )
    .expect("fts5 integrity-check failed: the index disagrees with the media table");
}

#[test]
fn the_upload_queue_rejects_duplicate_paths() {
    let conn = migrated();
    conn.execute(
        "INSERT INTO upload_queue (file_path, added_at) VALUES ('/library/a.jpg', 0)",
        [],
    )
    .unwrap();
    let second = conn.execute(
        "INSERT INTO upload_queue (file_path, added_at) VALUES ('/library/a.jpg', 0)",
        [],
    );
    assert!(
        second.is_err(),
        "the unique index did not stop a duplicate queue entry"
    );
}

/// Migration 20 has to collapse duplicates before it can add the constraint, or
/// the index fails to build and every existing library that queued the same file
/// twice under the old count-then-insert dedupe refuses to start.
///
/// Run against a library put back into the pre-constraint state, rather than a
/// hand-built schema, so the statements under test are the migration's own.
#[test]
fn collapsing_duplicate_queue_rows_keeps_the_oldest() {
    let conn = migrated();
    conn.execute_batch(
        "DROP INDEX idx_upload_queue_file_path;
         INSERT INTO upload_queue (file_path, status, added_at)
             VALUES ('/library/a.jpg', 'failed', 100),
                    ('/library/a.jpg', 'pending', 200),
                    ('/library/b.jpg', 'pending', 300);",
    )
    .unwrap();

    conn.execute_batch(
        "DELETE FROM upload_queue WHERE id NOT IN (
             SELECT MIN(id) FROM upload_queue GROUP BY file_path
         );
         CREATE UNIQUE INDEX IF NOT EXISTS idx_upload_queue_file_path
             ON upload_queue(file_path);",
    )
    .expect("the constraint should build once duplicates are collapsed");

    let rows: Vec<(String, String, i64)> = {
        let mut stmt = conn
            .prepare("SELECT file_path, status, added_at FROM upload_queue ORDER BY file_path")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(
        rows,
        vec![
            ("/library/a.jpg".to_string(), "failed".to_string(), 100),
            ("/library/b.jpg".to_string(), "pending".to_string(), 300),
        ]
    );
}

/// A row abandoned in `uploading` by a process that died is invisible to
/// `get_next_pending_item`, so the file never uploads and never reports an error.
#[test]
fn startup_requeues_uploads_stranded_by_a_previous_run() {
    let conn = migrated();
    conn.execute_batch(
        "INSERT INTO upload_queue (file_path, status, added_at)
             VALUES ('/library/a.jpg', 'uploading', 0),
                    ('/library/b.jpg', 'completed', 0),
                    ('/library/c.jpg', 'failed', 0);",
    )
    .unwrap();

    Database::migrate(&conn).unwrap();

    let status_of = |path: &str| -> String {
        conn.query_row(
            "SELECT status FROM upload_queue WHERE file_path = ?1",
            [path],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(status_of("/library/a.jpg"), "pending");
    assert_eq!(status_of("/library/b.jpg"), "completed");
    assert_eq!(status_of("/library/c.jpg"), "failed");
}

/// Render the schema of a migrated database as deterministic SQL.
///
/// Ordered by kind and then name rather than by `sqlite_master` order, because the
/// latter is the order the migrations happened to run in and would reshuffle the
/// whole file every time a column is added. Shadow tables that fts5 maintains for
/// itself are excluded: they are an implementation detail of the SQLite build, not
/// of this schema, and pinning them would turn a library upgrade into a failing
/// test.
fn schema_snapshot(conn: &Connection) -> String {
    let mut stmt = conn
        .prepare(
            "SELECT type, name, sql FROM sqlite_master
             WHERE sql IS NOT NULL
               AND name NOT LIKE 'sqlite_%'
               AND NOT (type = 'table' AND name LIKE 'media_fts_%')
             ORDER BY
                 CASE type
                     WHEN 'table' THEN 0
                     WHEN 'index' THEN 1
                     WHEN 'trigger' THEN 2
                     WHEN 'view' THEN 3
                     ELSE 4
                 END,
                 name",
        )
        .unwrap();
    let entries = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();

    let mut out = String::new();
    out.push_str(
        "-- Generated from the migration chain in src/database.rs. Do not edit by hand.\n\
         -- Refresh with: WANDERER_BLESS_SCHEMA=1 cargo test schema\n",
    );
    out.push_str(&format!(
        "-- PRAGMA user_version = {};\n",
        user_version(conn)
    ));
    for (_, _, sql) in entries {
        out.push('\n');
        out.push_str(&dedent(sql.trim()));
        out.push_str(";\n");
    }
    out
}

/// SQLite stores the original text of a statement, so every definition here comes
/// back indented to wherever it sat inside a Rust string literal. Strip that shared
/// prefix, or the snapshot reads as a ragged quotation of `database.rs` instead of
/// as a schema.
fn dedent(sql: &str) -> String {
    let common = sql
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    sql.lines()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 || line.trim().is_empty() {
                line.trim_end().to_string()
            } else {
                line[common..].trim_end().to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// #43: the schema existed only as ~500 lines of string literals spread across
/// twenty migration blocks, so nothing could be reviewed or diffed. This pins a
/// readable snapshot and fails when the chain and the snapshot disagree, which
/// also makes every schema change visible in the diff of the pull request that
/// makes it.
#[test]
fn the_committed_schema_matches_the_migration_chain() {
    let conn = migrated();
    let actual = schema_snapshot(&conn);
    let committed = include_str!("../../schema.sql");
    if actual == committed {
        return;
    }
    if std::env::var_os("WANDERER_BLESS_SCHEMA").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schema.sql");
        std::fs::write(&path, &actual).expect("write the refreshed schema snapshot");
        return;
    }
    panic!(
        "src-tauri/schema.sql is out of date with the migration chain.\n\
         Refresh it with: WANDERER_BLESS_SCHEMA=1 cargo test schema\n\
         and commit the result with the migration that changed it."
    );
}

static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A file-backed database, for the handful of tests that exercise `Database`
/// itself rather than the schema. Dropped with the directory.
struct TempDb {
    dir: PathBuf,
    db: Database,
}

impl TempDb {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("wanderer-db-test-{pid}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::new(dir.join("library.db")).expect("open the test database");
        Self { dir, db }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn queue_rows(db: &Database) -> Vec<(String, String, i64)> {
    let conn = db.get_conn().unwrap();
    let mut stmt = conn
        .prepare("SELECT file_path, status, retries FROM upload_queue ORDER BY file_path")
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    rows
}

/// The upsert replaced a count-then-insert, and it has to preserve what the count
/// did: queueing a path that is already waiting or in flight changes nothing.
#[test]
fn queueing_a_file_twice_leaves_the_first_entry_alone() {
    let tmp = TempDb::new();
    tmp.db.add_to_queue("/library/a.jpg").unwrap();
    tmp.db.add_to_queue("/library/a.jpg").unwrap();
    assert_eq!(
        queue_rows(&tmp.db),
        vec![("/library/a.jpg".to_string(), "pending".to_string(), 0)]
    );

    {
        let conn = tmp.db.get_conn().unwrap();
        conn.execute(
            "UPDATE upload_queue SET status = 'uploading' WHERE file_path = '/library/a.jpg'",
            [],
        )
        .unwrap();
    }
    tmp.db.add_to_queue("/library/a.jpg").unwrap();
    assert_eq!(
        queue_rows(&tmp.db),
        vec![("/library/a.jpg".to_string(), "uploading".to_string(), 0)],
        "an upload in flight was reset out from under the worker"
    );
}

/// The other half: a path that previously failed or completed is a genuine
/// re-queue, and must go back to pending with its retry count cleared.
#[test]
fn queueing_a_failed_file_again_resets_it_to_pending() {
    let tmp = TempDb::new();
    tmp.db.add_to_queue("/library/a.jpg").unwrap();
    {
        let conn = tmp.db.get_conn().unwrap();
        conn.execute(
            "UPDATE upload_queue SET status = 'failed', retries = 3, error_msg = 'boom'
             WHERE file_path = '/library/a.jpg'",
            [],
        )
        .unwrap();
    }

    tmp.db.add_to_queue("/library/a.jpg").unwrap();

    assert_eq!(
        queue_rows(&tmp.db),
        vec![("/library/a.jpg".to_string(), "pending".to_string(), 0)]
    );
    let error_msg: Option<String> = {
        let conn = tmp.db.get_conn().unwrap();
        conn.query_row(
            "SELECT error_msg FROM upload_queue WHERE file_path = '/library/a.jpg'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(error_msg, None, "the stale failure message survived");
}

/// The backup has to be a database, not a byte copy taken mid-write. With WAL on,
/// the rows below are in the log and not yet in library.db, so a file copy of the
/// main file would produce an empty library: exactly the failure being fixed.
#[test]
fn a_backup_is_a_complete_snapshot_of_the_live_database() {
    let tmp = TempDb::new();
    {
        let conn = tmp.db.get_conn().unwrap();
        insert_media(&conn, "/library/vacation.jpg");
        insert_media(&conn, "/library/invoices.pdf");
    }

    let dest = tmp.dir.join("backup.db");
    tmp.db.backup_to(&dest).expect("write the snapshot");

    let restored = Connection::open(&dest).unwrap();
    let paths: Vec<String> = {
        let mut stmt = restored
            .prepare("SELECT file_path FROM media ORDER BY file_path")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(
        paths,
        vec![
            "/library/invoices.pdf".to_string(),
            "/library/vacation.jpg".to_string()
        ]
    );
    assert_eq!(user_version(&restored), CURRENT_SCHEMA_VERSION);
    assert_eq!(fts_matches(&restored, "vacation").len(), 1);
}

/// Overwriting a backup silently would be worse than failing, so `VACUUM INTO`
/// refusing an existing target is load-bearing rather than incidental.
#[test]
fn a_backup_refuses_to_overwrite_an_existing_file() {
    let tmp = TempDb::new();
    let dest = tmp.dir.join("backup.db");
    std::fs::write(&dest, b"not a database").unwrap();

    assert!(tmp.db.backup_to(&dest).is_err());
    assert_eq!(std::fs::read(&dest).unwrap(), b"not a database");
}
