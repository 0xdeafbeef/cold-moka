use cold_moka::cached;
fn main() {}

#[cached]
pub async fn cached() -> i32 {
    1 + 2
}

#[cached]
pub async fn cached2(mut i8: i8) -> i32 {
    i8 += 1;
    let x = i8 + 3;
    x as i32
}

#[cached]
pub async fn cached3(mut i8: i8, _kek: u128) -> i32 {
    i8 += 1;
    let x = i8 + 3;
    x as i32
}

#[cached(ttl = "13")]
pub async fn cached4(mut i8: i8, _kek: u128) -> i32 {
    i8 += 1;
    let x = i8 + 3;
    x as i32
}

#[cached(ttl = "13", size = 1337)]
pub async fn cached5(mut i8: i8, _kek: u128) -> i32 {
    i8 += 1;
    let x = i8 + 3;
    x as i32
}

pub struct NoHash;

#[cached(ttl = "13", size = 1337, convert = "{i8}", key = "i8")]
pub async fn cached6(mut i8: i8, _ctx: NoHash) -> i32 {
    i8 += 1;
    let x = i8 + 3;
    x as i32
}

#[cached]
pub async fn result(inp: i32) -> Result<i32, i32> {
    Ok(inp)
}

#[cached]
async fn option(inp: i32) -> Option<i32> {
    Some(inp)
}

pub struct Wrapper<T>(T);

#[cached]
async fn destruct(Wrapper(aaaaaa): Wrapper<i32>) -> i32 {
    aaaaaa
}
