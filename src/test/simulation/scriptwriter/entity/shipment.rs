use fieldx::fxstruct;

#[derive(Clone, Debug)]
#[fxstruct(no_new, builder, get(copy))]
pub struct Shipment {
    product_id: i32,
    batch_size: i32,
    arrives_on: i32,
}
