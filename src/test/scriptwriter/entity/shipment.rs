use fieldx::fxstruct;

#[derive(Clone, Debug)]
#[fxstruct(no_new, builder, get(copy))]
pub struct Shipment {
    product_id: u32,
    batch_size: u32,
    arrives_on: u32,
}
