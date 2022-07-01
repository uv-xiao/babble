#![warn(
    clippy::all,
    clippy::pedantic,
    anonymous_parameters,
    elided_lifetimes_in_paths,
    missing_copy_implementations,
    missing_debug_implementations,
    single_use_lifetimes,
    trivial_casts,
    unreachable_pub,
    unused_lifetimes
)]
#![allow(clippy::non_ascii_literal)]

use babble::{
    ast_node::Expr,
    dreamcoder::{expr::DreamCoderOp, json::CompressionInput},
    experiments::{
        cache::ExperimentCache, BeamExperiment, EqsatExperiment, Experiment, Rounds, Summary,
    },
    rewrites,
};
use clap::Clap;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

#[allow(clippy::struct_excessive_bools)]
#[derive(Clap)]
#[clap(version, author, about)]
struct Opts {
    /// The input directory. If none is specified, defaults to `"harness/data/dreamcoder-benchmarks/benches"`.
    #[clap(parse(from_os_str))]
    file: Option<PathBuf>,

    #[clap(long)]
    domain: Option<String>,

    #[clap(long)]
    benchmark: Option<String>,

    #[clap(long)]
    cache: Option<PathBuf>,
}
const BENCHMARK_PATH: &str = "harness/data/dreamcoder-benchmarks/benches";
const DSR_PATH: &str = "harness/data/benchmark-dsrs";
const BEAM_SIZE: usize = 400;
const LPS: usize = 20;
const ROUNDS: usize = 1;
const MAX_ARITY: Option<usize> = Some(3);

#[derive(Debug)]
struct Benchmark<'a> {
    name: &'a str,
    path: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Iteration {
    domain: String,
    benchmark: String,
    file: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Compression {
    initial_size: usize,
    final_size: usize,
}

impl Compression {
    fn percent_improvement(&self, other: Self) -> f64 {
        100.0 * ((self.compression_ratio() / other.compression_ratio()) - 1.0)
    }

    fn compression_ratio(&self) -> f64 {
        self.initial_size as f64 / self.final_size as f64
    }
}

impl<'a, Op> From<&'a Summary<Op>> for Compression {
    fn from(summary: &'a Summary<Op>) -> Self {
        Self {
            initial_size: summary.initial_cost,
            final_size: summary.final_cost,
        }
    }
}

impl<'a, Op> From<&'a Option<Summary<Op>>> for Compression {
    fn from(summary: &'a Option<Summary<Op>>) -> Self {
        Self {
            initial_size: summary
                .as_ref()
                .map(|x| x.initial_cost)
                .unwrap_or_else(|| 1),
            final_size: summary.as_ref().map(|x| x.final_cost).unwrap_or_else(|| 1),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BenchResults {
    domain: String,
    benchmark: String,
    file: String,
    summary_first_eqsat: Option<Summary<DreamCoderOp>>,
    summary_all_eqsat: Option<Summary<DreamCoderOp>>,
    summary_first_none: Option<Summary<DreamCoderOp>>,
    summary_all_none: Option<Summary<DreamCoderOp>>,
    summary_first_dsrs: Option<Summary<DreamCoderOp>>,
    summary_all_dsrs: Option<Summary<DreamCoderOp>>,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let opts: Opts = Opts::parse();

    let mut cache = opts
        .cache
        .map_or_else(ExperimentCache::new, ExperimentCache::from_dir)?;

    println!("using cache: {}", cache.path().to_str().unwrap());

    let benchmark_path = opts.file.unwrap_or(PathBuf::from(BENCHMARK_PATH));

    let mut benchmark_dirs = Vec::new();
    for entry in fs::read_dir(&benchmark_path)? {
        let path = entry?.path();
        if fs::metadata(&path)?.is_dir() {
            benchmark_dirs.push(path);
        }
    }

    benchmark_dirs.sort_unstable();

    let mut domains: BTreeMap<_, Vec<_>> = BTreeMap::new();

    for benchmark_dir in &benchmark_dirs {
        let dir_name = benchmark_dir.file_name().unwrap().to_str().unwrap();
        let (domain, benchmark_name) = dir_name.split_once('_').unwrap();
        domains.entry(domain).or_default().push(Benchmark {
            name: benchmark_name,
            path: benchmark_dir.as_path(),
        });
    }

    println!("domains:");
    for (domain, benchmarks) in &domains {
        println!("  {}: {} benchmark(s)", domain, benchmarks.len());
    }

    if let Some(domain) = &opts.domain {
        run_domain(domain, &domains[domain.as_str()], &mut cache)?;
    } else {
        for (domain, benchmarks) in domains {
            run_domain(domain, &benchmarks, &mut cache)?;
        }
    }

    Ok(())
}

fn run_domain<'a, I>(
    domain: &'a str,
    benchmarks: I,
    cache: &mut ExperimentCache<DreamCoderOp>,
) -> anyhow::Result<()>
where
    I: IntoIterator<Item = &'a Benchmark<'a>>,
{
    let mut results = Vec::new();

    println!("domain: {}", domain);

    let dsr_file = PathBuf::from(DSR_PATH).join(format!("{}.rewrites", domain));
    let rewrites = rewrites::try_from_file(dsr_file)?.unwrap_or_default();

    println!("  found {} domain-specific rewrites", rewrites.len());

    for benchmark in benchmarks {
        println!("  benchmark: {}", benchmark.name);
        let mut inputs = Vec::new();

        for entry in fs::read_dir(benchmark.path)? {
            let path = entry?.path();
            if fs::metadata(&path)?.is_file() {
                inputs.push(path);
            }
        }

        inputs.sort_unstable();

        for input in &inputs {
            let file = input.file_name().unwrap().to_str().unwrap();

            println!("    file: {}", file);

            let mut summaries = BTreeMap::new();

            let input = fs::read_to_string(input)?;
            let input: CompressionInput = serde_json::from_str(&input)?;

            for use_all in [true, false] {
                // eqsat!
                {
                    let experiment_id = format!(
                        "{}-{}-{}-{}-eqsat",
                        &domain,
                        &benchmark.name,
                        file.strip_suffix(".json").unwrap(),
                        if use_all { "all" } else { "first" },
                    );

                    println!(
                        "      experiment{}: {}",
                        if cache.contains(&experiment_id) {
                            " [cached]"
                        } else {
                            ""
                        },
                        experiment_id
                    );

                    let program_groups: Vec<Vec<Expr<_>>> = input
                        .frontiers
                        .iter()
                        .cloned()
                        .map(|frontier| -> Vec<Expr<DreamCoderOp>> {
                            let programs = frontier
                                .programs
                                .into_iter()
                                .map(|program| program.program.into());

                            if use_all {
                                programs.collect()
                            } else {
                                programs.take(1).collect()
                            }
                        })
                        .collect();

                    let experiment = Rounds::new(1, EqsatExperiment::new(rewrites.clone(), ()));

                    let summary = cache.get_or_insert_with(&experiment_id, || {
                        experiment.run_multi_summary(program_groups)
                    })?;

                    summaries.insert((use_all, true, false, true), summary);
                }

                for use_dsrs in [true, false] {
                    for (lps, rounds) in [(ROUNDS, LPS)] {
                        let experiment_id = format!(
                            "{}-{}-{}-{}-{}-{}lps-{}rounds",
                            &domain,
                            &benchmark.name,
                            file.strip_suffix(".json").unwrap(),
                            if use_all { "all" } else { "first" },
                            if use_dsrs { "dsrs" } else { "none" },
                            lps,
                            rounds
                        );

                        println!(
                            "      experiment{}: {}",
                            if cache.contains(&experiment_id) {
                                " [cached]"
                            } else {
                                ""
                            },
                            experiment_id
                        );

                        let program_groups: Vec<Vec<Expr<_>>> = input
                            .frontiers
                            .iter()
                            .cloned()
                            .map(|frontier| -> Vec<Expr<DreamCoderOp>> {
                                let programs = frontier
                                    .programs
                                    .into_iter()
                                    .map(|program| program.program.into());

                                if use_all {
                                    programs.collect()
                                } else {
                                    programs.take(1).collect()
                                }
                            })
                            .collect();

                        let experiment = Rounds::new(
                            rounds,
                            BeamExperiment::new(
                                if use_dsrs { rewrites.clone() } else { vec![] },
                                BEAM_SIZE,
                                BEAM_SIZE,
                                lps,
                                false,
                                (),
                                true,
                                MAX_ARITY,
                            ),
                        );

                        let summary = cache.get_or_insert_with(&experiment_id, || {
                            experiment.run_multi_summary(program_groups)
                        })?;

                        summaries.insert((use_all, use_dsrs, lps == 1, false), summary);
                    }
                }
            }

            let bench_results = BenchResults {
                domain: domain.to_string(),
                benchmark: benchmark.name.to_string(),
                file: file.to_string(),
                summary_first_eqsat: summaries.remove(&(false, true, false, true)),
                summary_all_eqsat: summaries.remove(&(true, true, false, true)),
                summary_first_none: summaries.remove(&(false, false, true, false)),
                summary_all_none: summaries.remove(&(true, false, true, false)),
                summary_first_dsrs: summaries.remove(&(false, true, true, false)),
                summary_all_dsrs: summaries.remove(&(true, true, true, false)),
            };

            results.push(bench_results);
        }
    }

    plot_raw_data(&results)?;
    plot_costs(&results)?;
    plot_dsr_impact(&results)?;
    plot_compression(&results)?;

    Ok(())
}

fn plot_raw_data(results: &[BenchResults]) -> anyhow::Result<()> {
    let mut csv_writer = csv::Writer::from_path("harness/data_gen/raw_costs.csv")?;
    csv_writer.serialize((
        "benchmark",
        "initial",
        "dc",
        "first eqsat",
        "all eqsat",
        "first none",
        "first dsrs",
        "all none",
        "all dsrs",
    ))?;

    let dc_results = get_dc_results()?;

    for result in results {
        let iteration = Iteration {
            domain: result.domain.clone(),
            benchmark: result.benchmark.clone(),
            file: result.file.clone(),
        };
        let dc_compression = dc_results[&iteration];

        csv_writer.serialize((
            format!("{}_{}/{}", result.domain, result.benchmark, result.file),
            dc_compression.initial_size,
            dc_compression.final_size,
            result
                .summary_first_eqsat
                .as_ref()
                .map(|x| x.final_cost)
                .unwrap_or_else(|| 0),
            result
                .summary_all_eqsat
                .as_ref()
                .map(|x| x.final_cost)
                .unwrap_or_else(|| 0),
            result
                .summary_first_none
                .as_ref()
                .map(|x| x.final_cost)
                .unwrap_or_else(|| 0),
            result
                .summary_first_dsrs
                .as_ref()
                .map(|x| x.final_cost)
                .unwrap_or_else(|| 0),
            result
                .summary_all_none
                .as_ref()
                .map(|x| x.final_cost)
                .unwrap_or_else(|| 0),
            result
                .summary_all_dsrs
                .as_ref()
                .map(|x| x.final_cost)
                .unwrap_or_else(|| 0),
        ))?;
    }

    csv_writer.flush()?;
    Ok(())
}

fn plot_costs(results: &[BenchResults]) -> anyhow::Result<()> {
    let mut csv_writer = csv::Writer::from_path("harness/data_gen/costs.csv")?;
    csv_writer.serialize((
        "benchmark",
        "first eqsat",
        "all eqsat",
        "first none",
        "first dsrs",
        "all none",
        "all dsrs",
    ))?;

    for result in results {
        csv_writer.serialize((
            format!("{}_{}/{}", result.domain, result.benchmark, result.file),
            result
                .summary_first_eqsat
                .as_ref()
                .map(|x| x.space_saving_percentage())
                .unwrap_or_else(|| 0.0),
            result
                .summary_all_eqsat
                .as_ref()
                .map(|x| x.space_saving_percentage())
                .unwrap_or_else(|| 0.0),
            result
                .summary_first_none
                .as_ref()
                .map(|x| x.space_saving_percentage())
                .unwrap_or_else(|| 0.0),
            result
                .summary_first_dsrs
                .as_ref()
                .map(|x| x.space_saving_percentage())
                .unwrap_or_else(|| 0.0),
            result
                .summary_all_none
                .as_ref()
                .map(|x| x.space_saving_percentage())
                .unwrap_or_else(|| 0.0),
            result
                .summary_all_dsrs
                .as_ref()
                .map(|x| x.space_saving_percentage())
                .unwrap_or_else(|| 0.0),
        ))?;
    }

    csv_writer.flush()?;
    Ok(())
}

fn plot_dsr_impact(results: &[BenchResults]) -> anyhow::Result<()> {
    let mut csv_writer = csv::Writer::from_path("harness/data_gen/dsr-impact.csv")?;
    csv_writer.serialize(("benchmark", "percent improvement"))?;

    for (i, result) in results.iter().enumerate() {
        if let Some(x) = &result.summary_first_dsrs {
            if let Some(y) = &result.summary_first_none {
                let percent_improved = x.percent_improved(&y);
                csv_writer.serialize((i, percent_improved))?;
            }
        }
    }

    csv_writer.flush()?;
    Ok(())
}

fn plot_compression(results: &[BenchResults]) -> anyhow::Result<()> {
    let dc_results = get_dc_results()?;

    let mut csv_writer = csv::Writer::from_path("harness/data_gen/compression.csv")?;
    csv_writer.serialize((
        "all dsrs/all none",
        "first dsrs/first none",
        "all dsrs/first dsrs",
        "all none/first none",
        "all dsrs/dc",
        "all none/dc",
    ))?;

    for result in results {
        let iteration = Iteration {
            domain: result.domain.clone(),
            benchmark: result.benchmark.clone(),
            file: result.file.clone(),
        };
        let dc_compression = dc_results[&iteration];
        //assert_eq!(dc_result.initial_cost, result.summary_all_none.initial_cost, "cost mismatch: {:?}", &iteration);

        let all_dsrs = Compression::from(&result.summary_all_dsrs);
        let all_none = Compression::from(&result.summary_all_none);
        let first_dsrs = Compression::from(&result.summary_first_dsrs);
        let first_none = Compression::from(&result.summary_first_none);

        csv_writer.serialize((
            all_dsrs.percent_improvement(all_none),
            first_dsrs.percent_improvement(first_none),
            all_dsrs.percent_improvement(first_dsrs),
            all_none.percent_improvement(first_none),
            all_dsrs.percent_improvement(dc_compression),
            all_none.percent_improvement(dc_compression),
        ))?;
    }

    csv_writer.flush()?;
    Ok(())
}

fn get_dc_results() -> anyhow::Result<HashMap<Iteration, Compression>> {
    let mut dc_results = HashMap::new();

    let mut csv_reader = csv::Reader::from_path("harness/data_gen/dc_res.csv")?;

    for record in csv_reader.records() {
        let record = record?;
        let (domain, benchmark) = record[0].split_once('_').unwrap();
        let file = record[1].to_string();
        let initial_size: usize = record[2].parse()?;
        let final_size: usize = record[3].parse()?;

        dc_results.insert(
            Iteration {
                domain: domain.to_string(),
                benchmark: benchmark.to_string(),
                file,
            },
            Compression {
                initial_size,
                final_size,
            },
        );
    }

    Ok(dc_results)
}
