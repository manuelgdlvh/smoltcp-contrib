
pub struct UmemAreaConfig {
    pub entries: usize,
    pub alignment: ChunkAlignment,
}

#[derive(Copy, Clone)]
pub enum ChunkAlignment {
    TwoK,
    FourK,
}

impl From<ChunkAlignment> for usize {
    fn from(value: ChunkAlignment) -> Self {
        match value {
            ChunkAlignment::TwoK => 2048,
            ChunkAlignment::FourK => 4096,
        }
    }
}
