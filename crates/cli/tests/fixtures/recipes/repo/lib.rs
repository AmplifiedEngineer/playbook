//! Shipping costs for the demo shop.

/// A parcel to ship.
pub struct Parcel {
    pub weight_grams: u32,
}

/// Returns the shipping price in cents for a parcel.
pub fn shipping_cents(parcel: &Parcel) -> u32 {
    let base = 299;
    base + parcel.weight_grams / 100 * 10
}

/// Returns true when the parcel ships free.
pub fn ships_free(parcel: &Parcel) -> bool {
    shipping_cents(parcel) < 100
}
