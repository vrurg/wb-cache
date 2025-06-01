use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "session_migration"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Sessions::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Sessions::Id).big_integer().not_null().primary_key())
                    .col(ColumnDef::new(Sessions::CustomerId).integer().null())
                    .col(ColumnDef::new(Sessions::ExpiresOn).integer().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-sessions-customer_id")
                            .from(Sessions::Table, Sessions::CustomerId)
                            .to(super::customer::Customers::Table, super::customer::Customers::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Sessions::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
pub enum Sessions {
    Table,
    Id,
    CustomerId,
    ExpiresOn,
}
