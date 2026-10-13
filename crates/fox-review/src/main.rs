use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    fox_review::run(fox_review::parse_review_options(&args)?).await
}
