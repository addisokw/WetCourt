use rand::seq::SliceRandom;

const CHARGES: &[&str] = &[
    "You stand accused of pronouncing 'gif' with a hard G.",
    "You are charged with reply-all on a company-wide email.",
    "You stand accused of leaving a single dish in the sink for over 72 hours.",
    "You are charged with adding semicolons to Python code.",
    "You stand accused of pushing directly to main without code review.",
];

pub fn random() -> String {
    CHARGES.choose(&mut rand::thread_rng()).unwrap().to_string()
}
