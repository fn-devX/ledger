use ledger_application::ports::IdGenerator;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Default)]
pub struct UuidV7Generator;

impl IdGenerator for UuidV7Generator {
    fn generate(&self) -> Uuid {
        Uuid::now_v7()
    }
}
