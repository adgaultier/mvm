//! Generated sandbox names: `<adjective>-<animal>` (docker-style), used when a
//! sandbox is created without an explicit `--name`.

/// Adjectives for generated sandbox names.
pub const ADJECTIVES: &[&str] = &[
    "agile", "alert", "ancient", "brave", "clever", "curious", "daring", "elusive", "elegant",
    "fierce", "fearless", "gentle", "golden", "graceful", "keen", "lively", "majestic", "mighty",
    "nimble", "playful", "proud", "restless", "roaming", "rugged", "silent", "soaring", "stealthy",
    "swift", "untamed", "vibrant", "watchful", "wily",
];

/// Savannah animals for generated sandbox names.
pub const SAVANNAH_ANIMALS: &[&str] = &[
    "aardvark",
    "aardwolf",
    "agama",
    "baboon",
    "bohor",
    "buffalo",
    "bushbuck",
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
///
/// No external rng: the word picks come from a xorshift64 seeded with the
/// wall clock, so the crate keeps its zero-heavy-dependency budget.
pub fn random_sandbox_name<F>(taken: F) -> String
where
    F: Fn(&str) -> bool,
{
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0) as u64;
    for _ in 0..128 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let name = format!(
            "{}-{}",
            ADJECTIVES[(seed & 0xffff) as usize % ADJECTIVES.len()],
            SAVANNAH_ANIMALS[(seed >> 16) as usize % SAVANNAH_ANIMALS.len()]
        );
        if !taken(&name) {
            return name;
        }
    }
    // Only reachable if every adjective-animal pair is taken; don't spin
    // forever, just disambiguate with a numeric suffix.
    let mut n = 0u32;
    loop {
        let name = format!(
            "{}-{}-{n}",
            ADJECTIVES[(seed & 0xffff) as usize % ADJECTIVES.len()],
            SAVANNAH_ANIMALS[(seed >> 16) as usize % SAVANNAH_ANIMALS.len()]
        );
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
        assert!(SAVANNAH_ANIMALS.contains(&animal));
    }

    #[test]
    fn skips_taken_names() {
        let taken = ["bold-lion", "clever-zebra"];
        let name = random_sandbox_name(|n| taken.contains(&n));
        assert!(!taken.contains(&name.as_str()));
    }
}
