use std::time::Duration;
use sqlx::{migrate::MigrateDatabase, sqlite::{SqliteConnectOptions, SqliteJournalMode}, SqlitePool};
use anyhow::Result;
use crate::models::{CreateInputRequest, InputRow, OutputRow, input_kind_string};

pub async fn init_database() -> Result<SqlitePool> {
    let db_url = "sqlite://./state.db";
    
    if !sqlx::Sqlite::database_exists(db_url).await? {
        sqlx::Sqlite::create_database(db_url).await?;
    }

    let conn_opts = SqliteConnectOptions::new()
                    .filename("state.db")
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal)   // ← WAL
                    .busy_timeout(Duration::from_secs(5));  // ← 5 s reintento

    // Connect to the database
    let db = SqlitePool::connect_with(conn_opts).await?;

    // Migrate the database
    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .expect("migraciones fallaron");

    Ok(db)
}

pub async fn save_input_to_db(
    pool: &SqlitePool,
    name: Option<&str>,
    request: &CreateInputRequest,
) -> Result<i64> {
    let kind = input_kind_string(request);
    let config_json = serde_json::to_string(request)?;

    let result = sqlx::query(
        "INSERT INTO inputs (name, kind, config_json, status) VALUES (?, ?, ?, 'listening')"
    )
    .bind(name)
    .bind(kind)
    .bind(config_json)
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}

pub async fn delete_input_from_db(pool: &SqlitePool, id: i64) -> Result<()> {
    // Delete outputs first (should cascade, but explicit for safety)
    sqlx::query("DELETE FROM outputs WHERE input_id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    
    // Delete input
    sqlx::query("DELETE FROM inputs WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    
    Ok(())
}

pub async fn save_output_to_db(
    pool: &SqlitePool,
    name: Option<&str>,
    input_id: i64,
    kind: &str,
    destination: &str,
    config_json: Option<&str>,
    listen_port: Option<u16>,
) -> Result<i64> {
    let result = sqlx::query(
        "INSERT INTO outputs (name, input_id, kind, destination, config_json, listen_port, status) VALUES (?, ?, ?, ?, ?, ?, 'connecting')"
    )
    .bind(name)
    .bind(input_id)
    .bind(kind)
    .bind(destination)
    .bind(config_json)
    .bind(listen_port.map(|p| p as i32))
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}

pub async fn delete_output_from_db(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query("DELETE FROM outputs WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    
    Ok(())
}

pub async fn get_all_inputs(pool: &SqlitePool) -> Result<Vec<InputRow>> {
    let rows = sqlx::query_as::<_, InputRow>("SELECT id, name, kind, config_json, status FROM inputs")
        .fetch_all(pool)
        .await?;

    Ok(rows)
}

pub async fn get_all_outputs(pool: &SqlitePool) -> Result<Vec<OutputRow>> {
    println!("Consultando todos los outputs en la base de datos...");
    let rows = sqlx::query_as::<_, OutputRow>(
        "SELECT id, name, input_id, kind, destination, config_json, listen_port, status FROM outputs"
    )
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

pub async fn check_output_exists(
    pool: &SqlitePool,
    input_id: i64,
    destination: &str,
) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outputs WHERE input_id = ? AND destination = ?"
    )
    .bind(input_id)
    .bind(destination)
    .fetch_one(pool)
    .await?;

    Ok(count > 0)
}

pub async fn check_port_conflict(
    pool: &SqlitePool,
    listen_port: u16,
    exclude_output_id: Option<i64>,
) -> Result<bool> {
    let query = match exclude_output_id {
        Some(exclude_id) => {
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM outputs WHERE listen_port = ? AND id != ?"
            )
            .bind(listen_port as i32)
            .bind(exclude_id)
        },
        None => {
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM outputs WHERE listen_port = ?"
            )
            .bind(listen_port as i32)
        }
    };

    let count: i64 = query.fetch_one(pool).await?;
    Ok(count > 0)
}

pub async fn get_input_by_id(pool: &SqlitePool, input_id: i64) -> Result<Option<InputRow>> {
    let row = sqlx::query_as::<_, InputRow>(
        "SELECT id, name, kind, config_json, status FROM inputs WHERE id = ?"
    )
    .bind(input_id)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

pub async fn get_output_by_id(pool: &SqlitePool, output_id: i64) -> Result<Option<OutputRow>> {
    let row = sqlx::query_as::<_, OutputRow>(
        "SELECT id, name, input_id, kind, destination, config_json, listen_port, status FROM outputs WHERE id = ?"
    )
    .bind(output_id)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

// Status update functions
pub async fn update_input_status_in_db(pool: &SqlitePool, input_id: i64, status: &str) -> Result<()> {
    sqlx::query("UPDATE inputs SET status = ? WHERE id = ?")
        .bind(status)
        .bind(input_id)
        .execute(pool)
        .await?;

    Ok(())
}

pub async fn update_output_status_in_db(pool: &SqlitePool, output_id: i64, status: &str) -> Result<()> {
    sqlx::query("UPDATE outputs SET status = ? WHERE id = ?")
        .bind(status)
        .bind(output_id)
        .execute(pool)
        .await?;

    Ok(())
}

pub async fn get_input_id_for_output(pool: &SqlitePool, output_id: i64) -> Result<i64> {
    let row: (i64,) = sqlx::query_as("SELECT input_id FROM outputs WHERE id = ?")
        .bind(output_id)
        .fetch_one(pool)
        .await?;

    Ok(row.0)
}

pub async fn update_input_in_db(
    pool: &SqlitePool,
    id: i64,
    name: Option<&str>,
    config_json: &str,
) -> Result<()> {
    sqlx::query("UPDATE inputs SET name = ?, config_json = ? WHERE id = ?")
        .bind(name)
        .bind(config_json)
        .bind(id)
        .execute(pool)
        .await?;

    Ok(())
}

pub async fn update_output_in_db(
    pool: &SqlitePool,
    id: i64,
    name: Option<&str>,
    destination: &str,
    config_json: Option<&str>,
    listen_port: Option<u16>,
) -> Result<()> {
    sqlx::query("UPDATE outputs SET name = ?, destination = ?, config_json = ?, listen_port = ? WHERE id = ?")
        .bind(name)
        .bind(destination)
        .bind(config_json)
        .bind(listen_port.map(|p| p as i32))
        .bind(id)
        .execute(pool)
        .await?;

    Ok(())
}