use crate::gcal::{check_token_validity, get_oauth_token, get_start_end_time};
use chrono::{DateTime, Duration, FixedOffset, NaiveDateTime, NaiveTime};
use clap::Parser;
use futures::future::join_all;
use gcal::{get_user_calender, CalendarEvent, TimeWrapper};
use pagerduty::{get_pagerduty_schedule, FinalPagerDutySchedule};
use reqwest::{self, Client};
use std::iter::zip;
use std::{env, fs};
use tabled::{Table, Tabled};

mod gcal;
mod pagerduty;
mod webserver;

/// Pagerduty and google calendar conflict resolver
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// date string to start from, in the form of YYYY-mm-dd
    #[clap(short, long, value_parser)]
    start_date: String,
    #[clap(short, long, value_parser)]
    duration_days: i64,
    #[clap(short, long, value_parser)]
    pd_schedule: String,
}

#[tokio::main]
async fn main() -> Result<(), String> {
    // Environment variables
    const PD_API_KEY: &str = "PD_API_KEY";
    const GOOGLE_CLIENT_ID: &str = "GOOGLE_CLIENT_ID";
    const GOOGLE_CLIENT_SECRET: &str = "GOOGLE_CLIENT_SECRET";

    let api_key = env::var(PD_API_KEY).expect(&format!(
        "Expected environment variable {} to be set",
        PD_API_KEY
    ));
    let google_client_id = env::var(GOOGLE_CLIENT_ID).expect(&format!(
        "Expected environment variable {} to be set",
        GOOGLE_CLIENT_ID
    ));
    let google_client_secret = env::var(GOOGLE_CLIENT_SECRET).expect(&format!(
        "Expected environment variable {} to be set",
        GOOGLE_CLIENT_SECRET
    ));

    // Command line args
    let args = Args::parse();
    let start_date = args.start_date;
    let duration_days = args.duration_days;
    let pd_schedule_id = args.pd_schedule;

    let (start_time, end_time) = get_start_end_time(&start_date, duration_days);

    let client = reqwest::Client::new();

    // Google
    let token_file = ".google_oidc_token";
    let retrieved_token = match fs::read_to_string(token_file) {
        Err(_e) => {
            println!(
                "Local token file {} not found. Triggering oauth flow.",
                &token_file
            );
            get_oauth_token(&google_client_id, &google_client_secret).await
        }
        Ok(value) => Ok(value),
    };

    let token = match retrieved_token {
        Ok(inside) => inside,
        Err(e) => return Err(e.to_string()),
    };

    // check token expiry and trigger oauth if expired
    let token = match check_token_validity(&client, &token).await {
        Err(e) if &e == "Unauthorised" => {
            println!("Unauthorised. Trying to get new token.");
            match get_oauth_token(&google_client_id, &google_client_secret).await {
                Ok(inside) => inside,
                Err(e) => return Err(e.to_string()),
            }
        }
        Err(e) => return Err(e.to_string()),
        Ok(_) => token,
    };
    fs::write(token_file, &token).expect("Unable to write token file");

    //pagerduty
    let pd_schedule =
        get_pagerduty_schedule(&client, api_key, pd_schedule_id, start_time, end_time).await;

    let sg_am_shift: Vec<FinalPagerDutySchedule> = pd_schedule
        .clone()
        .into_iter()
        .filter(|schedule| {
            schedule.start.time() == NaiveTime::from_hms(7, 0, 0)
                && schedule.end.time() == NaiveTime::from_hms(15, 0, 0)
        })
        .collect();

    let sg_pm_shift: Vec<FinalPagerDutySchedule> = pd_schedule
        .into_iter()
        .filter(|schedule| {
            schedule.start.time() == NaiveTime::from_hms(15, 0, 0)
                && schedule.end.time() == NaiveTime::from_hms(23, 0, 0)
        })
        .collect();

    let available_shifts_futures = vec![(sg_am_shift, "AM"), (sg_pm_shift, "PM")]
        .into_iter()
        .map(|(shift, shift_type)| {
            get_available_shifts_per_user(
                shift,
                &client,
                &token,
                start_time,
                end_time,
                duration_days,
                shift_type,
            )
        });

    // let available_shifts: Vec<(FinalPagerDutySchedule, Vec<OncallSlot>)> =
    let current_shifts: Vec<FinalEntity> = join_all(available_shifts_futures)
        .await
        .into_iter()
        .flatten()
        .collect();

    println!("Total number of shifts: {}", current_shifts.len());

    let unavailable_folks: Vec<ZeroSwaps> = current_shifts
        .clone()
        .into_iter()
        .filter(|shift| shift.available_slots.len() == 0)
        .map(|x| convert_to_zero_swaps(x.pd_schedule))
        .collect();
    if unavailable_folks.len() > 0 {
        println!(
            "\n========Folks with zero swaps found. Please remove them from the pd schedule======="
        );
        println!("{}", Table::new(unavailable_folks).to_string());
        return Err("Folks with zero slots available".to_string());
    };

    let (rescheduled_shifts, swaps) = recursive_solution(&current_shifts, Vec::new());
    // TODO: Util function to print this properly
    println!(
        "\n========Simulating swaps. Note that these are sequential and stateful=============="
    );
    println!("{}", Table::new(swaps).to_string());

    // TODO: Print this as a table for readability
    let final_overrides = print_diff_of_shift(current_shifts, rescheduled_shifts);
    println!("\n====Generating final diff against current schedule======");
    println!("{}", Table::new(final_overrides).to_string());

    return Ok(());
}

// Final displays for table
#[derive(Tabled)]
struct ZeroSwaps {
    email: String,
    start: String,
    end: String,
}

fn convert_to_zero_swaps(input: FinalPagerDutySchedule) -> ZeroSwaps {
    ZeroSwaps {
        email: input.email,
        start: input.start.format("%c").to_string(),
        end: input.end.format("%c").to_string(),
    }
}

#[derive(Tabled, Debug, Clone)]
struct SimulatedSwap {
    person_with_conflict: String,
    original_slot: String,
    swapped_with: String,
    new_slot: String,
}

#[derive(Tabled)]
struct FinalOverride {
    original_slot: String,
    original_assignee: String,
    final_override: String,
}

// End

#[derive(Debug, Clone)]
struct FinalEntity {
    pd_schedule: FinalPagerDutySchedule,
    available_slots: Vec<OncallSlot>,
}

impl PartialEq for FinalEntity {
    fn eq(&self, other: &Self) -> bool {
        self.pd_schedule.email == other.pd_schedule.email
            && self.pd_schedule.start == other.pd_schedule.start
            && self.pd_schedule.end == other.pd_schedule.end
    }
}

fn recursive_solution(
    schedule: &Vec<FinalEntity>,
    mut swaps: Vec<SimulatedSwap>,
) -> (Vec<FinalEntity>, Vec<SimulatedSwap>) {
    let (most_restrictive_option, rest) = find_conflicts(schedule);
    // println!("most restrictive conflict: {:?}", &most_restrictive_option);

    // if this doesn't exist, we assume it's already solved and this is the termination condition. else, proceed
    let most_restrict_conflict = match most_restrictive_option {
        None => return (schedule.clone(), swaps), // termination condition
        Some(value) => {
            assert_eq!(rest.len(), schedule.len() - 1);
            value
        }
    };

    // find best swap from remaining entries in schedule, and remove that from the list
    let (best_swap_option, after_swap) =
        find_potential_swap(&most_restrict_conflict, &rest, swaps.clone());
    // println!("best swap: {:?}", &best_swap_option);
    let best_swap = match best_swap_option {
        None => todo!(), // should panic? no swaps
        Some(value) => {
            assert_eq!(after_swap.len(), rest.len() - 1);
            value
        }
    };

    // apply swap
    let source_modified = FinalEntity {
        pd_schedule: FinalPagerDutySchedule {
            start: best_swap.pd_schedule.start,
            end: best_swap.pd_schedule.end,
            email: most_restrict_conflict.pd_schedule.email.clone(),
        },
        available_slots: most_restrict_conflict.clone().available_slots,
    };
    // println!("original conflicter: {:?}", most_restrict_conflict);
    // println!("after modifed: {:?}", source_modified);
    let destination_modified = FinalEntity {
        pd_schedule: FinalPagerDutySchedule {
            start: most_restrict_conflict.pd_schedule.start,
            end: most_restrict_conflict.pd_schedule.end,
            email: best_swap.pd_schedule.email.clone(),
        },
        available_slots: best_swap.clone().available_slots,
    };
    // println!("original to swap: {:?}", best_swap);
    // println!("swap modifed: {:?}", destination_modified);

    let mut schedule_after_swapping = after_swap;
    schedule_after_swapping.push(source_modified);
    schedule_after_swapping.push(destination_modified);
    assert_eq!(schedule_after_swapping.len(), schedule.len());
    swaps.push(SimulatedSwap {
        person_with_conflict: most_restrict_conflict.pd_schedule.email,
        original_slot: most_restrict_conflict
            .pd_schedule
            .start
            .format("%c")
            .to_string(),
        swapped_with: best_swap.pd_schedule.email,
        new_slot: best_swap.pd_schedule.start.format("%c").to_string(),
    });
    if swaps.len() > 100 {
        for swap in swaps {
            println!("{:?}", swap);
        }

        panic!("Unable to find solution")
    }
    // println!("{}", &swap_string);
    return recursive_solution(&schedule_after_swapping, swaps);
}

/// find the most restrictive conflict, and return: (most_restrictive_conflict, rest_with_conflict_removed)
fn find_conflicts(available_shifts: &Vec<FinalEntity>) -> (Option<FinalEntity>, Vec<FinalEntity>) {
    let (mut remaining_pool, mut conflict_pool) =
        available_shifts
            .into_iter()
            .fold((Vec::new(), Vec::new()), |acc, x| {
                let current_slot = (&x.pd_schedule).clone();
                let available_slots = (&x.available_slots).clone();
                let mut pool = acc.0;
                let mut conflicts = acc.1;
                if has_conflicts(&current_slot, &available_slots) {
                    conflicts.push(FinalEntity {
                        pd_schedule: current_slot,
                        available_slots,
                    });
                } else {
                    pool.push(FinalEntity {
                        pd_schedule: current_slot,
                        available_slots,
                    });
                }
                return (pool, conflicts);
            });
    conflict_pool.sort_by(|a, b| a.available_slots.len().cmp(&b.available_slots.len()));
    // remove first conflict and put the rest back into the pool
    match conflict_pool.split_first() {
        Some((most_restrictive, rest)) => {
            let mut to_move = rest.to_vec();
            remaining_pool.append(&mut to_move);
            return (Some(most_restrictive.clone()), remaining_pool);
        }
        None => (None, remaining_pool),
    }

    // return conflict_pool;
}

fn find_potential_swap(
    // current_slot: &FinalPagerDutySchedule,
    current_slot: &FinalEntity,
    all_slots: &Vec<FinalEntity>,
    swaps: Vec<SimulatedSwap>,
) -> (Option<FinalEntity>, Vec<FinalEntity>) {
    let mut potential_swaps: Vec<FinalEntity> = current_slot
        .clone()
        .available_slots
        .into_iter()
        .map(|available_slot| {
            all_slots.into_iter().filter(move |slot| {
                slot.pd_schedule.start == available_slot.start_time
                    && slot.pd_schedule.end == available_slot.end_time
            })
        })
        .flatten()
        .cloned()
        .collect();
    potential_swaps.sort_by(|a, b| a.available_slots.len().cmp(&b.available_slots.len()));
    let last_swap = swaps.last();
    if last_swap.is_some() {
        // println!("last_swap: {:?}", &last_swap);
        // Remove the last swap from the pool to avoid a cyclic error
        potential_swaps = potential_swaps
            .into_iter()
            .filter(|x| x.pd_schedule.email != last_swap.unwrap().person_with_conflict)
            .collect();
    };
    if swaps.len() >= 2 {
        let last_last_swap = swaps.get(&swaps.len() - 2);
        // println!("last_last_swap: {:?}", &last_last_swap);
        if last_last_swap.is_some() {
            potential_swaps = potential_swaps
                .into_iter()
                .filter(|x| x.pd_schedule.email != last_last_swap.unwrap().person_with_conflict)
                .collect();
        }
    }
    // brute force for now and loop through another time
    // TODO: Write the above as a fold and avoid another loop
    let mut remaining_pool: Vec<FinalEntity> = all_slots
        .into_iter()
        .filter(|slot| !potential_swaps.contains(&slot))
        .cloned()
        .collect();
    match potential_swaps.split_first() {
        Some((best_swap, rest)) => {
            let mut to_move = rest.to_vec();
            remaining_pool.append(&mut to_move);
            return (Some(best_swap.clone()), remaining_pool);
        }
        None => (None, remaining_pool),
    }
    // return potential_swaps;
}

async fn get_available_shifts_per_user(
    shifts: Vec<FinalPagerDutySchedule>,
    client: &Client,
    token: &str,
    start_time_local: DateTime<FixedOffset>,
    end_time_local: DateTime<FixedOffset>,
    duration_days: i64,
    shift_type: &str,
) -> Vec<FinalEntity> {
    let futures = shifts.into_iter().map(|user_pd| {
        get_user_calender(&client, user_pd, &token, start_time_local, end_time_local)
    });

    let results: Vec<(FinalPagerDutySchedule, Vec<CalendarEvent>)> =
        join_all(futures).await.into_iter().collect();

    // availble oncall slots
    let available_oncalls: Vec<FinalEntity> = results
        .into_iter()
        .map(|(user, user_events)| FinalEntity {
            pd_schedule: user,
            available_slots: get_available_slots(
                user_events,
                shift_type,
                start_time_local.date().format("%Y-%m-%d").to_string(),
                duration_days,
            ),
        })
        .collect();
    return available_oncalls;
}

#[derive(Debug, Clone)]
struct OncallSlot {
    start_time: DateTime<FixedOffset>,
    end_time: DateTime<FixedOffset>,
}

fn get_oncall_slots(shift_type: &str, start_date: String, duration_days: i64) -> Vec<OncallSlot> {
    let start_time = match shift_type {
        x if x == "AM".to_string() => "07:00",
        x if x == "PM".to_string() => "15:00",
        _ => "error",
    };
    let sgt_timezone = FixedOffset::east(8 * 60 * 60);
    let start_datetime_string = format!("{} {}", start_date, start_time);
    // println!("{}", &start_datetime_string);
    let start_time = NaiveDateTime::parse_from_str(&start_datetime_string, "%Y-%m-%d %H:%M")
        .expect(&format!("Error parsing {}", &start_datetime_string));
    let start_time_local = DateTime::<FixedOffset>::from_local(start_time, sgt_timezone);
    let mut final_vec = Vec::new();
    for i in 0..duration_days {
        let shift_start_time = start_time_local
            .checked_add_signed(Duration::days(i))
            .unwrap();
        let shift_end_time = shift_start_time
            .checked_add_signed(Duration::hours(8))
            .unwrap();
        let slot = OncallSlot {
            start_time: shift_start_time,
            end_time: shift_end_time,
        };
        final_vec.push(slot);
    }
    return final_vec;
}

// For every user, generate a list of "available shifts"
fn get_available_slots(
    user_events: Vec<CalendarEvent>,
    shift_type: &str,
    start_date: String,
    duration_days: i64,
) -> Vec<OncallSlot> {
    let slots = get_oncall_slots(shift_type, start_date, duration_days);
    let available_slots: Vec<OncallSlot> = slots
        .into_iter()
        .filter(|oncall_slot| !slot_clashes(oncall_slot, &user_events))
        .collect();
    return available_slots;
}

fn slot_clashes(oncall_slot: &OncallSlot, events: &Vec<CalendarEvent>) -> bool {
    for event in events {
        let event_start = convert_time_wrapper(event.start.as_ref().unwrap());
        let event_end = convert_time_wrapper(event.end.as_ref().unwrap());
        let oncall_start = oncall_slot.start_time;
        let oncall_end = oncall_slot.end_time;
        //https://stackoverflow.com/questions/325933/determine-whether-two-date-ranges-overlap
        if event_start <= oncall_end && event_end >= oncall_start {
            return true;
        }
    }
    return false;
}

fn convert_time_wrapper(input: &TimeWrapper) -> DateTime<FixedOffset> {
    let standard_format = "%Y-%m-%d %H:%M";
    let sgt_timezone = FixedOffset::east(8 * 60 * 60);
    let final_time = match input.date_string.clone() {
        Some(value) => {
            let naive = NaiveDateTime::parse_from_str(&format!("{} 00:00", value), standard_format)
                .unwrap();
            DateTime::<FixedOffset>::from_local(naive, sgt_timezone)
        }
        None => {
            let x = input.date_time_string.clone().unwrap();
            DateTime::<FixedOffset>::parse_from_rfc3339(&x).unwrap()
        }
    };
    return final_time;
}

/// find conflicts. I.e. his initial scheduled slot is not in the vector of available slots a person has
fn has_conflicts(current_slot: &FinalPagerDutySchedule, available_slots: &Vec<OncallSlot>) -> bool {
    return available_slots
        .into_iter()
        .filter(|slot| slot.start_time == current_slot.start && slot.end_time == current_slot.end)
        .count()
        == 0;
}

/// Get diff a shift. A loop of a loop, pretty inefficient
/// Can be made better by pre-sorting both and zipping?
fn print_diff_of_shift(
    mut initial_shifts: Vec<FinalEntity>,
    mut final_shifts: Vec<FinalEntity>,
) -> Vec<FinalOverride> {
    let mut final_overrides = Vec::new();
    // println!("\n====Generating final diff against current schedule======");
    initial_shifts.sort_by(|a, b| a.pd_schedule.start.cmp(&b.pd_schedule.start));
    final_shifts.sort_by(|a, b| a.pd_schedule.start.cmp(&b.pd_schedule.start));
    let zipped = zip(initial_shifts, final_shifts);
    for pair in zipped {
        let (original, new) = pair;
        assert!(original.pd_schedule.start == new.pd_schedule.start);
        if original.pd_schedule.email != new.pd_schedule.email {
            // let formatted_time = original.pd_schedule.start.format("%A %-I%p (%Y-%m-%d)");
            // println!(
            //     "Replace {} at slot {} with {}",
            //     original.pd_schedule.email, formatted_time, new.pd_schedule.email
            // );
            final_overrides.push(FinalOverride {
                original_assignee: original.pd_schedule.email,
                original_slot: original.pd_schedule.start.format("%c").to_string(),
                final_override: new.pd_schedule.email,
            });
        }
    }
    return final_overrides;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_oncall_slot() {
        let slots = get_oncall_slots("AM", "2022-08-22".to_string(), 14);
        assert!(slots.len() == 14);
        let first = slots.first().unwrap();
        assert_eq!(
            first.start_time.to_string(),
            "2022-08-22 07:00:00 +08:00".to_string()
        );
        assert_eq!(
            first.end_time.to_string(),
            "2022-08-22 15:00:00 +08:00".to_string()
        );
        let last = slots.last().unwrap();
        assert_eq!(
            last.start_time.to_string(),
            "2022-09-04 07:00:00 +08:00".to_string()
        );
        assert_eq!(
            last.end_time.to_string(),
            "2022-09-04 15:00:00 +08:00".to_string()
        );
    }

    #[test]
    fn test_find_conflicts_false() {
        let current_pd_shift = FinalPagerDutySchedule {
            start: DateTime::<FixedOffset>::parse_from_rfc3339("2022-08-30T07:00:00+08:00")
                .unwrap(),
            end: DateTime::<FixedOffset>::parse_from_rfc3339("2022-08-30T15:00:00+08:00").unwrap(),
            email: "random.user@grabtaxi.com".to_string(),
        };
        let oncall_slots = vec![
            OncallSlot {
                start_time: DateTime::<FixedOffset>::parse_from_rfc3339(
                    "2022-08-30T07:00:00+08:00",
                )
                .unwrap(),
                end_time: DateTime::<FixedOffset>::parse_from_rfc3339("2022-08-30T15:00:00+08:00")
                    .unwrap(),
            },
            OncallSlot {
                start_time: DateTime::<FixedOffset>::parse_from_rfc3339(
                    "2022-08-31T07:00:00+08:00",
                )
                .unwrap(),
                end_time: DateTime::<FixedOffset>::parse_from_rfc3339("2022-08-31T15:00:00+08:00")
                    .unwrap(),
            },
        ];
        let result = has_conflicts(&current_pd_shift, &oncall_slots);
        assert_eq!(result, false);
    }

    #[test]
    fn test_find_conflicts() {
        let current_pd_shift = FinalPagerDutySchedule {
            start: DateTime::<FixedOffset>::parse_from_rfc3339("2022-08-30T07:00:00+08:00")
                .unwrap(),
            end: DateTime::<FixedOffset>::parse_from_rfc3339("2022-08-30T15:00:00+08:00").unwrap(),
            email: "random.user@grabtaxi.com".to_string(),
        };
        let oncall_slots = vec![
            OncallSlot {
                start_time: DateTime::<FixedOffset>::parse_from_rfc3339(
                    "2022-08-29T07:00:00+08:00",
                )
                .unwrap(),
                end_time: DateTime::<FixedOffset>::parse_from_rfc3339("2022-08-29T15:00:00+08:00")
                    .unwrap(),
            },
            OncallSlot {
                start_time: DateTime::<FixedOffset>::parse_from_rfc3339(
                    "2022-08-31T07:00:00+08:00",
                )
                .unwrap(),
                end_time: DateTime::<FixedOffset>::parse_from_rfc3339("2022-08-31T15:00:00+08:00")
                    .unwrap(),
            },
        ];
        let result = has_conflicts(&current_pd_shift, &oncall_slots);
        assert_eq!(result, true);
    }

    #[test]
    fn test_recursive_solution_base_case() {
        let schedule = vec![
            FinalEntity {
                pd_schedule: FinalPagerDutySchedule {
                    start: DateTime::<FixedOffset>::parse_from_rfc3339("2022-08-30T07:00:00+08:00")
                        .unwrap(),
                    end: DateTime::<FixedOffset>::parse_from_rfc3339("2022-08-30T15:00:00+08:00")
                        .unwrap(),
                    email: "random.user@grabtaxi.com".to_string(),
                },
                available_slots: vec![
                    OncallSlot {
                        start_time: DateTime::<FixedOffset>::parse_from_rfc3339(
                            "2022-08-29T07:00:00+08:00",
                        )
                        .unwrap(),
                        end_time: DateTime::<FixedOffset>::parse_from_rfc3339(
                            "2022-08-29T15:00:00+08:00",
                        )
                        .unwrap(),
                    },
                    OncallSlot {
                        start_time: DateTime::<FixedOffset>::parse_from_rfc3339(
                            "2022-08-31T07:00:00+08:00",
                        )
                        .unwrap(),
                        end_time: DateTime::<FixedOffset>::parse_from_rfc3339(
                            "2022-08-31T15:00:00+08:00",
                        )
                        .unwrap(),
                    },
                ],
            },
            FinalEntity {
                pd_schedule: FinalPagerDutySchedule {
                    start: DateTime::<FixedOffset>::parse_from_rfc3339("2022-08-31T07:00:00+08:00")
                        .unwrap(),
                    end: DateTime::<FixedOffset>::parse_from_rfc3339("2022-08-31T15:00:00+08:00")
                        .unwrap(),
                    email: "random.user2@grabtaxi.com".to_string(),
                },
                available_slots: vec![
                    OncallSlot {
                        start_time: DateTime::<FixedOffset>::parse_from_rfc3339(
                            "2022-08-29T07:00:00+08:00",
                        )
                        .unwrap(),
                        end_time: DateTime::<FixedOffset>::parse_from_rfc3339(
                            "2022-08-29T15:00:00+08:00",
                        )
                        .unwrap(),
                    },
                    OncallSlot {
                        start_time: DateTime::<FixedOffset>::parse_from_rfc3339(
                            "2022-08-30T07:00:00+08:00",
                        )
                        .unwrap(),
                        end_time: DateTime::<FixedOffset>::parse_from_rfc3339(
                            "2022-08-30T15:00:00+08:00",
                        )
                        .unwrap(),
                    },
                ],
            },
        ];

        let (rescheduled, swaps) = recursive_solution(&schedule, Vec::new());
        println!("\n========Simulating swaps==============");
        println!("{}", Table::new(swaps).to_string());

        let final_overrides = print_diff_of_shift(schedule, rescheduled);
        println!("\n====Generating final diff against current schedule======");
        println!("{}", Table::new(final_overrides).to_string());
    }
}
