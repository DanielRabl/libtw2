extern crate clap;
extern crate common;
extern crate datafile;
extern crate logger;
extern crate map;
extern crate rmp;

use common::num::Cast;
use map::format::Tile;
use std::fs::File;
use std::io;
use std::path::Path;
use std::process;

#[derive(Debug)]
struct Error(map::Error);

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error(e.into())
    }
}

impl From<datafile::Error> for Error {
    fn from(e: datafile::Error) -> Error {
        Error(e.into())
    }
}

impl From<map::format::Error> for Error {
    fn from(e: map::format::Error) -> Error {
        Error(e.into())
    }
}

impl From<map::Error> for Error {
    fn from(e: map::Error) -> Error {
        Error(e)
    }
}

impl From<rmp::encode::ValueWriteError> for Error {
    fn from(e: rmp::encode::ValueWriteError) -> Error {
        use rmp::encode::ValueWriteError::*;
        match e {
            InvalidDataWrite(e) => Error(e.into()),
            InvalidMarkerWrite(e) => Error(e.into()),
        }
    }
}

fn count<'a, I: Iterator<Item=&'a Tile>>(tiles: I, count: &mut [u64; 256]) {
    for tile in tiles {
        count[tile.index.usize()] += 1;
    }
}

fn tile(index: u8) -> Option<&'static str> {
    Some(match index {
        12 => "DFREEZE",
        16 => "WALLJUMP",
        17 => "EHOOK_START",
        19 => "HIT_START",
        21 => "SOLO_START",
        
        96 => "TELE_GUN",
        104 => "NPC_START",
        105 => "SUPER_START",
        106 => "JETPACK_START",
        107 => "NPH_START",
        112 => "TELE_GRENADE",
        128 => "TELE_LASER",

        199 => "WEAPON_SHOTGUN",
        200 => "WEAPON_GRENADE",
        201 => "POWERUP_NINJA",
        202 => "WEAPON_RIFLE",

        _ => return None,
    })
}

fn process(path: &Path, output_path: &Path) -> Result<(), Error> {
    let mut output = File::create(output_path)?;
    let mut map = map::Reader::open(path)?;
    let game_layers = map.game_layers()?;

    let mut tiles_count = [0u64; 256];
    count(map.layer_tiles(game_layers.game())?.iter(), &mut tiles_count);
    if let Some(f) = game_layers.front() {
        count(map.layer_tiles(f)?.iter(), &mut tiles_count);
    }

    rmp::encode::write_uint(&mut output, game_layers.width.u64())?;
    rmp::encode::write_uint(&mut output, game_layers.height.u64())?;

    let len = tiles_count.iter().enumerate().filter(|&(i, &c)| {
        c != 0 && tile(i.assert_u8()).is_some()
    }).count();

    rmp::encode::write_map_len(&mut output, len.assert_u32())?;
    for (i, &c) in tiles_count.iter().enumerate() {
        if c == 0 {
            continue;
        }
        if let Some(desc) = tile(i.assert_u8()) {
            rmp::encode::write_str(&mut output, desc)?;
            rmp::encode::write_bool(&mut output, true)?;
        }
    }

    Ok(())
}

fn main() {
    use clap::App;
    use clap::Arg;

    logger::init();

    let matches = App::new("DDNet map properties extractor")
        .about("Reads a map file and reports width/height of the game layer and\
                some of its contents, in msgpack format.")
        .arg(Arg::with_name("MAP")
             .help("Sets the map file to analyse")
             .required(true))
        .arg(Arg::with_name("OUTPUT")
             .help("Sets the msgpack file to output")
             .required(true))
        .get_matches();

    let path = Path::new(matches.value_of_os("MAP").unwrap());
    let output_path = Path::new(matches.value_of_os("OUTPUT").unwrap());

    match process(path, output_path) {
        Ok(()) => {},
        Err(err) => {
            println!("{}: {:?}", path.display(), err);
            process::exit(1);
        }
    }
}
