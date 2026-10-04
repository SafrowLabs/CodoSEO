//! `codoseo migrate`: apply every pending Postgres migration from `DATABASE_URL`.

use clap::Args;

use super::{CliError, EXIT_OK, Outcome};

#[derive(Debug, Args)]
pub struct MigrateArgs {}

pub async fn run(_args: MigrateArgs) -> Outcome {
    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| CliError::msg("DATABASE_URL is not set"))?;
    let pool = codoseo_store::pool::connect(&database_url)
        .await
        .map_err(|e| CliError::msg(format!("could not connect to Postgres: {e}")))?;
    codoseo_store::pool::migrate(&pool)
        .await
        .map_err(|e| CliError::msg(format!("migration failed: {e}")))?;
    println!("migrations applied");
    Ok(EXIT_OK)
}
