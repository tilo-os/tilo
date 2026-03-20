use vello::peniko::Color;

fn main() {
    let c = Color::rgb8(30, 30, 35);
    println!("rgb8(30,30,35) = {:?}", c);
    
    let c2 = Color::rgba8(30, 30, 35, 255);
    println!("rgba8(30,30,35, 255) = {:?}", c2);
}
