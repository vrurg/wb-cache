use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "customer_migration"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Customers::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Customers::Id).integer().not_null().primary_key())
                    .col(ColumnDef::new(Customers::Email).string().not_null())
                    .col(ColumnDef::new(Customers::FirstName).string().not_null())
                    .col(ColumnDef::new(Customers::LastName).string().not_null())
                    .col(ColumnDef::new(Customers::RegisteredOn).integer().not_null())
                    .index(Index::create().name("idx-unique-email").col(Customers::Email).unique())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Customers::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
pub enum Customers {
    Table,
    Id,
    Email,
    FirstName,
    LastName,
    RegisteredOn,
}
