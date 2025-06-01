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
                    .col(ColumnDef::new(Orders::ProductId).integer().not_null())
                    .col(ColumnDef::new(Orders::Quantity).integer().not_null())
                    .col(ColumnDef::new(Orders::Status).string().not_null())
                    .col(ColumnDef::new(Orders::PurchasedOn).integer().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-orders-customer_id")
                            .from(Orders::Table, Orders::CustomerId)
                            .to(super::customer::Customers::Table, super::customer::Customers::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-orders-product_id")
                            .from(Orders::Table, Orders::ProductId)
                            .to(super::product::Products::Table, super::product::Products::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
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
pub enum Orders {
    Table,
    Id,
    CustomerId,
    ProductId,
    Quantity,
    Status,
    PurchasedOn,
}
