//! Generated sandbox names: `<adjective>-<animal>` (docker-style), used when a
//! sandbox is created without an explicit `--name`.

/// Adjectives for generated sandbox names.
pub const ADJECTIVES: &[&str] = &[
    "bold",
    "brave",
    "calm",
    "clever",
    "curious",
    "daring",
    "elegant",
    "fierce",
    "gentle",
    "golden",
    "graceful",
    "lively",
    "mighty",
    "nimble",
    "playful",
    "proud",
    "quick",
    "quiet",
    "restless",
    "steady",
    "stealthy",
    "sturdy",
    "swift",
    "vibrant",
    "watchful",
    "wild",
];

/// Savannah animals for generated sandbox names.
pub const SAVANNAH_ANIMALS: &[&str] = &[
    "lion",
    "leopard",
    "cheetah",
    "elephant",
    "giraffe",
    "zebra",
    "wildebeest",
    "buffalo",
    "hippopotamus",
    "rhinoceros",
    "hyena",
    "jackal",
    "wild_dog",
    "meerkat",
    "mongoose",
    "warthog",
    "baboon",
    "vervet_monkey",
    "colobus_monkey",
    "ostrich",
    "secretary_bird",
    "marabou_stork",
    "saddle_billed_stork",
    "crowned_crane",
    "hornbill",
    "vulture",
    "kori_bustard",
    "serval",
    "caracal",
    "african_wildcat",
    "aardvark",
    "aardwolf",
    "pangolin",
    "porcupine",
    "springbok",
    "impala",
    "gazelle",
    "eland",
    "kudu",
    "oryx",
    "sable_antelope",
    "roan_antelope",
    "hartebeest",
    "topi",
    "waterbuck",
    "bushbuck",
    "duiker",
    "dik_dik",
    "gerenuk",
    "gnu",
    "ratel",
    "civet",
    "genet",
    "nyala",
    "oribi",
    "klipspringer",
    "reedbuck",
    "oxpecker",
    "guineafowl",
    "kestrel",
    "chameleon",
    "agama",
    "crocodile",
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
