//! Benchmark seeder: populate a vault with N synthetic jots via
//! direct batched commits (bypasses the one-commit-per-create
//! protocol path — this is a load generator, not a cuj client).
//!
//!     cargo run --release --example seed -- <vault-root> <n>

use cuj::binding;
use cuj::facts::{self, Birth, CounterBody, Ident, to_fact};
use cuj::reads::Reads;
use metatheca::{Fact, FsMetadata, Vault};

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().expect("usage: seed <vault-root> <n>");
    let n: i64 = args
        .next()
        .expect("usage: seed <vault-root> <n>")
        .parse()
        .unwrap();

    let vault = Vault::open(&root).expect("vault (run `cuj init` against it first)");
    let profile = facts::ProfileConfig::new("default");
    let reads = Reads::load(&vault, None).unwrap();
    let profile_entry = reads.profile_entry("default").unwrap();
    let mut next_id = reads.next_id("default").unwrap();

    const BATCH: i64 = 100;
    let mut created = 0i64;
    while created < n {
        let mut batch = Vec::new();
        let count = BATCH.min(n - created);
        for _ in 0..count {
            let id = next_id;
            next_id += 1;
            let content = format!(
                "seed note {id} about topic-{} ..tag{} ::area::sub{} lorem ipsum {}\n",
                id % 173,
                id % 23,
                id % 11,
                id * 2654435761u32 as i64 % 100000,
            );
            let result = binding::parse(&content, &profile);
            let parsed = binding::to_parsed_index(&result, "default", &|_, _| None);
            let ident = Ident {
                profile: "default".into(),
                id,
                ext: "md".into(),
            };
            let birth = Birth {
                profile: "default".into(),
                id,
                ext: "md".into(),
                created_at_ns: facts::now_ns(),
            };
            let entry = vault.new_entry_id();
            let blob = vault.deposit_blob(content.as_bytes()).unwrap();
            batch.push(to_fact(facts::K_CUJ_JOT, entry, &birth).unwrap());
            batch.push(to_fact(facts::K_CUJ_ID, entry, &ident).unwrap());
            batch.push(to_fact(facts::K_JOT_INDEX, entry, &parsed).unwrap());
            batch.push(Fact::blob_ref(entry, &blob.to_hex(), content.len() as u64));
            batch.push(Fact::path(entry, &format!("default/{id}.md"), true));
            batch.push(Fact::fs_metadata(entry, &FsMetadata::default(), None));
            batch.push(Fact::mime(entry, "text/markdown"));
        }
        batch.push(
            to_fact(
                facts::K_CUJ_COUNTER,
                profile_entry,
                &CounterBody { value: next_id - 1 },
            )
            .unwrap(),
        );
        vault.commit(&batch).unwrap();
        created += count;
        if created % 1000 == 0 {
            eprintln!("{created}/{n}");
        }
    }
    eprintln!("seeded {created} jots (next id {next_id})");
}
