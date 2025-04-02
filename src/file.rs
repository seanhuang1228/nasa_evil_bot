use tokio::{fs::File, io::AsyncReadExt};

pub async fn dump_file(filename: &str) -> Result<String, std::io::Error> {
    let mut config = File::open(filename).await?;
    let mut content = String::new();
    config.read_to_string(&mut content).await?;
    Ok(content)
}
