use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "order_migration"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Orders::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Orders::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Orders::CustomerId).integer().not_null())
                    .col(ColumnDef::new(Orders::Total).decimal().not_null())
                    .col(ColumnDef::new(Orders::PurchasedOn).integer().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-orders-purchased_on")
                    .table(Orders::Table)
                    .col(Orders::PurchasedOn)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.drop_table(Table::drop().table(Orders::Table).to_owned()).await
    }
}

#[derive(Iden)]
enum Orders {
    Table,
    Id,
    CustomerId,
    Total,
    PurchasedOn,
}
