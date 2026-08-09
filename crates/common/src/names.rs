//! Generated sandbox names: `<adjective>-<animal>` (docker-style), used when a
//! sandbox is created without an explicit `--name`.

/// Adjectives for generated sandbox names.
pub const ADJECTIVES: &[&str] = &[
    "agile", "alert", "ancient", "brave", "clever", "curious", "daring", "elusive", "elegant",
    "fierce", "fearless", "gentle", "golden", "graceful", "keen", "lively", "majestic", "mighty",
    "nimble", "playful", "proud", "restless", "roaming", "rugged", "silent", "soaring", "stealthy",
    "swift", "untamed", "vibrant", "watchful", "wily",
];

/// Cool animals for generated sandbox names.
pub const COOL_ANIMALS: &[&str] = &[
    "aardvark",
    "aardwolf",
    "agama",
    "baboon",
    "bohor",
    "buffalo",
    "bushbuck",
    "capybara", // thanks to pyth0ps for his positive contribution to the projet
    "caracal",
    "chacma",
    "chameleon",
    "cheetah",
    "civet",
    "colobus",
    "crocodile",
    "crowned_crane",
    "duiker",
    "eland",
    "elephant",
    "gemsbok",
    "genet",
    "gerenuk",
    "giraffe",
    "gnu",
    "ground_hornbill",
    "guineafowl",
    "hartebeest",
    "hippopotamus",
    "hyena",
    "impala",
    "jackal",
    "kestrel",
    "klipspringer",
    "kudu",
    "lion",
    "marabou_stork",
    "meerkat",
    "mongoose",
    "nyala",
    "oribi",
    "oryx",
    "ostrich",
    "oxpecker",
    "pangolin",
    "painted_wolf",
    "porcupine",
    "ratel",
    "reedbuck",
    "rhinoceros",
    "roan",
    "sable",
    "secretary_bird",
    "serval",
    "springbok",
    "tamanoir",
    "topi",
    "vervet",
    "vulture",
    "warthog",
    "waterbuck",
    "zebra",
];

/// Pick a `<adjective>-<animal>` name that `taken` does not already reject.
pub fn random_sandbox_name<F>(taken: F) -> String
where
    F: Fn(&str) -> bool,
{
    use rand::Rng;
    let mut rng = rand::thread_rng();
    for _ in 0..128 {
        let name = format!(
            "{}-{}",
            ADJECTIVES[rng.gen_range(0..ADJECTIVES.len())],
            COOL_ANIMALS[rng.gen_range(0..COOL_ANIMALS.len())]
        );
        if !taken(&name) {
            return name;
        }
    }
    // Only reachable if every adjective-animal pair is taken; don't spin
    // forever, just disambiguate with a numeric suffix.
    let (adj, animal) = (
        ADJECTIVES[rng.gen_range(0..ADJECTIVES.len())],
        COOL_ANIMALS[rng.gen_range(0..COOL_ANIMALS.len())],
    );
    let mut n = 0u32;
    loop {
        let name = format!("{adj}-{animal}-{n}");
        if !taken(&name) {
            return name;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_an_adjective_animal_pair() {
        let name = random_sandbox_name(|_| false);
        let (adj, animal) = name.split_once('-').unwrap();
        assert!(ADJECTIVES.contains(&adj));
        assert!(COOL_ANIMALS.contains(&animal));
    }

    #[test]
    fn skips_taken_names() {
        let taken = ["bold-lion", "clever-zebra"];
        let name = random_sandbox_name(|n| taken.contains(&n));
        assert!(!taken.contains(&name.as_str()));
    }
}
