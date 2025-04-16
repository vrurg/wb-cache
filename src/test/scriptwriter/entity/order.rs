use fieldx::fxstruct;
use uuid::Uuid;

use crate::test::types::OrderStatus;

#[fxstruct(no_new, builder, get(copy))]
#[derive(Clone, Debug)]
pub struct Order {
    /// The order id.
    pub id:          Uuid,
    /// The customer id.
    pub customer_id: u32,
    /// The product id.
    pub product_id:  u32,
    /// The quantity of the product.
    pub quantity:    u32,
    /// The status of the order.
    pub status:      OrderStatus,
}

impl From<Order> for crate::test::db::entity::Order {
    fn from(order: Order) -> Self {
        Self {
            id:           order.id,
            customer_id:  order.customer_id,
            product_id:   order.product_id,
            quantity:     order.quantity,
            status:       order.status,
            purchased_on: 0, // Placeholder, as this field is not present in the struct
        }
    }
}
