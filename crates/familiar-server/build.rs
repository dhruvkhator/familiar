fn main() {
    // sqlx::migrate! embeds the SQL files at compile time.
    println!("cargo:rerun-if-changed=../../migrations");
}
