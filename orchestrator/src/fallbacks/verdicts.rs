use rand::Rng;

use crate::state_machine::states::Verdict;

pub fn random(guilty_bias: f64) -> Verdict {
    let guilty = rand::thread_rng().gen::<f64>() < guilty_bias;
    if guilty {
        Verdict {
            guilty: true,
            intensity: 3,
            deliberation: "Having weighed the defendant's woeful plea against the gravity of the offense, the bench finds the matter open and shut.".into(),
            remarks: "Justice, as ever, is wet.".into(),
        }
    } else {
        Verdict {
            guilty: false,
            intensity: 0,
            deliberation: "The defendant's argument, while irregular, possesses an unexpected charm. The court is grudgingly amused.".into(),
            remarks: "Acquitted. Do not let it happen again.".into(),
        }
    }
}
