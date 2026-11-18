//! How the app draws what the model only names: the icon a category, a
//! maneuver or a place kind shows, chosen from the toolkit's icons.

use ui::icons::Icon;

use crate::model::{Category, Maneuver};

pub fn category_icon(category: Category) -> Icon {
    match category {
        Category::Landmark => Icon::Flag,
        Category::Cafe => Icon::Coffee,
        Category::Restaurant => Icon::Utensils,
        Category::Bar => Icon::Beer,
        Category::Museum => Icon::Landmark,
        Category::Park => Icon::Trees,
        Category::Transit => Icon::TrainFront,
        Category::Hotel => Icon::BedDouble,
        Category::Shop => Icon::ShoppingBag,
        Category::Hospital => Icon::Hospital,
        Category::Other => Icon::MapPin,
    }
}

pub fn maneuver_icon(maneuver: Maneuver) -> Icon {
    match maneuver {
        Maneuver::Depart | Maneuver::Continue | Maneuver::Merge => Icon::ArrowUp,
        Maneuver::SlightLeft => Icon::ArrowUpLeft,
        Maneuver::Left => Icon::CornerUpLeft,
        Maneuver::SharpLeft => Icon::ArrowDownLeft,
        Maneuver::SlightRight => Icon::ArrowUpRight,
        Maneuver::Right => Icon::CornerUpRight,
        Maneuver::SharpRight => Icon::ArrowDownRight,
        Maneuver::Uturn => Icon::Undo2,
        Maneuver::Roundabout => Icon::RotateCw,
        Maneuver::Arrive => Icon::Flag,
    }
}
