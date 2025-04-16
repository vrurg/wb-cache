use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(InventoryRecords::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(InventoryRecords::ProductId)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(InventoryRecords::Stock).integer().not_null())
                    .col(
                        ColumnDef::new(InventoryRecords::HandlingDays)
                            .tiny_unsigned() // using tiny unsigned for u8 type
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(InventoryRecords::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(Iden)]
enum InventoryRecords {
    Table,
    ProductId,
    Stock,
    HandlingDays,
}
