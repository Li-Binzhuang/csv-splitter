use super::*;

#[cfg(all(test, unix))]
mod mcp_job_shutdown_tests {
    use super::*;

    #[test]
    fn shutdown_kills_a_running_job_and_removes_its_files() {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let prefix = format!("csvx-mcp-shutdown-{}", std::process::id());
        let stdout = std::env::temp_dir().join(format!("{prefix}.out"));
        let stderr = std::env::temp_dir().join(format!("{prefix}.err"));
        std::fs::write(&stdout, b"partial").unwrap();
        std::fs::write(&stderr, b"partial").unwrap();
        let mut jobs = std::collections::HashMap::new();
        jobs.insert(
            "job-running".to_string(),
            McpJob {
                child,
                stdout: stdout.clone(),
                stderr: stderr.clone(),
                result: None,
                started: std::time::Instant::now(),
            },
        );
        shutdown_mcp_jobs(&mut jobs);
        let job = jobs.get_mut("job-running").expect("job retained");
        assert!(
            job.child.try_wait().unwrap().is_some(),
            "running child should be reaped"
        );
        assert!(!stdout.exists());
        assert!(!stderr.exists());
    }
}

#[cfg(all(test, unix))]
mod mcp_job_retention_tests {
    use super::*;

    #[test]
    fn prune_keeps_the_cap_and_deletes_the_oldest_files() {
        let mut jobs = std::collections::HashMap::new();
        let prefix = format!("csvx-mcp-prune-{}", std::process::id());
        for index in 0..=MCP_JOB_HISTORY {
            let mut child = std::process::Command::new("true")
                .spawn()
                .expect("spawn true");
            child.wait().expect("wait true");
            let stdout = std::env::temp_dir().join(format!("{prefix}-{index}.out"));
            let stderr = std::env::temp_dir().join(format!("{prefix}-{index}.err"));
            std::fs::write(&stdout, b"ok").unwrap();
            std::fs::write(&stderr, b"").unwrap();
            jobs.insert(
                format!("job-{index}"),
                McpJob {
                    child,
                    stdout,
                    stderr,
                    result: Some((true, 0, "ok".to_string())),
                    started: std::time::Instant::now(),
                },
            );
        }
        assert_eq!(jobs.len(), MCP_JOB_HISTORY + 1);
        // Called just before an insert, pruning leaves one free slot.
        prune_mcp_jobs(&mut jobs);
        assert_eq!(jobs.len(), MCP_JOB_HISTORY - 1);
        let remaining = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
            .count();
        assert_eq!(remaining, (MCP_JOB_HISTORY - 1) * 2);
        let mut child = std::process::Command::new("true").spawn().unwrap();
        child.wait().unwrap();
        let stdout = std::env::temp_dir().join(format!("{prefix}-new.out"));
        let stderr = std::env::temp_dir().join(format!("{prefix}-new.err"));
        std::fs::write(&stdout, b"ok").unwrap();
        std::fs::write(&stderr, b"").unwrap();
        jobs.insert(
            "job-new".to_string(),
            McpJob {
                child,
                stdout,
                stderr,
                result: Some((true, 0, "ok".to_string())),
                started: std::time::Instant::now(),
            },
        );
        assert_eq!(jobs.len(), MCP_JOB_HISTORY);
        for job in jobs.values() {
            drop_mcp_job_files(job);
        }
    }
}

#[cfg(test)]
mod moved_cache_tests {
    use super::*;

    #[test]
    fn startup_cleanup_removes_dead_materialized_files() {
        let dir = std::env::temp_dir();
        let dead = dir.join("csvx-materialized-4294967295-1.csv");
        let live = dir.join(format!("csvx-materialized-{}-test.csv", std::process::id()));
        std::fs::write(&dead, "x").unwrap();
        std::fs::write(&live, "x").unwrap();
        let removed = cleanup_stale_mcp_materialized();
        assert_eq!(removed, 1);
        assert!(!dead.exists());
        assert!(live.exists());
        std::fs::remove_file(live).ok();
    }

    #[test]
    fn cache_evicts_lru_when_limit_is_exceeded() {
        let dir = std::env::temp_dir().join(format!("csvs_cache_evict_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.csv");
        let second = dir.join("second.csv");
        std::fs::write(&first, "a\n1\n").unwrap();
        std::fs::write(&second, "a\n2\n").unwrap();
        let mut entries = Vec::new();
        cache_insert(
            &mut entries,
            CachedMaterialized {
                key: "first".to_string(),
                path: first.clone(),
                size: 10,
            },
            100,
            1,
        );
        cache_insert(
            &mut entries,
            CachedMaterialized {
                key: "second".to_string(),
                path: second.clone(),
                size: 10,
            },
            100,
            1,
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "second");
        assert!(!first.exists());
        assert!(second.exists());
        cleanup_mcp_materialized();
        std::fs::remove_file(&second).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cache_lookup_moves_a_hit_to_the_back() {
        let dir = std::env::temp_dir().join(format!("csvs_cache_lru_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.csv");
        let second = dir.join("second.csv");
        let third = dir.join("third.csv");
        for path in [&first, &second, &third] {
            std::fs::write(path, "a\n1\n").unwrap();
        }
        let mut entries = Vec::new();
        for (key, path) in [("first", &first), ("second", &second)] {
            cache_insert(
                &mut entries,
                CachedMaterialized {
                    key: key.to_string(),
                    path: path.clone(),
                    size: 10,
                },
                100,
                2,
            );
        }
        assert_eq!(cache_lookup(&mut entries, "first"), Some(first.clone()));
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.key.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
        cache_insert(
            &mut entries,
            CachedMaterialized {
                key: "third".to_string(),
                path: third.clone(),
                size: 10,
            },
            100,
            2,
        );
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.key.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "third"]
        );
        assert!(!second.exists());
        assert!(first.exists());
        assert!(third.exists());
        cleanup_mcp_materialized();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn materialize_cache_reuses_one_temp_file() {
        let dir = std::env::temp_dir().join(format!("csvs_materialize_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("orders.csv");
        std::fs::write(&source, "id,city\n1,CDMX\n").unwrap();
        let gzip = dir.join("orders.csv.gz");
        let status = std::process::Command::new("gzip")
            .arg("-c")
            .arg(&source)
            .stdout(std::fs::File::create(&gzip).unwrap())
            .status();
        if !status.map(|status| status.success()).unwrap_or(false) {
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
        let first = mcp_materialize(&gzip).unwrap();
        let second = mcp_materialize(&gzip).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            std::fs::read_to_string(&first).unwrap(),
            "id,city\n1,CDMX\n"
        );
        cleanup_mcp_materialized();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mcp_tools_is_valid_json_and_has_all_tools() {
        let value = McpParser::parse(mcp_tools()).expect("tools/list must be valid JSON");
        let McpJson::Array(tools) = value else {
            panic!("tools/list must be an array");
        };
        assert_eq!(tools.len(), 23);
        for tool in &tools {
            let McpJson::Object(pairs) = tool else {
                panic!("tool must be an object");
            };
            assert!(pairs.iter().any(|(key, _)| key == "name"));
            assert!(pairs.iter().any(|(key, _)| key == "inputSchema"));
        }
        assert!(mcp_tools().contains(
        r#""name":"csvx_cache_clear","description":"Delete cached materialized temp files, clear the cache and sweep dead-process temp files; returns removed/stale counts and bytes.","annotations":{"destructiveHint":true,"idempotentHint":true}"#
    ));
        assert!(mcp_tools().contains(
        r#""name":"csvx_cache_stats","description":"Materialize-cache and read-window hits/misses, entries and byte usage in this server process.","annotations":{"readOnlyHint":true}"#
    ));
    }
}

#[test]
fn command_mapping_rejects_invalid_types_and_bounds() {
    let view = McpJson::Object(vec![
        ("path".to_string(), McpJson::String(".".to_string())),
        ("count".to_string(), McpJson::Number(0.0)),
    ]);
    assert!(crate::executor::mcp_command(Path::new("."), "csvx_view", &view).is_err());

    let pipeline = McpJson::Object(vec![
        ("path".to_string(), McpJson::String("plan.csv".to_string())),
        ("force".to_string(), McpJson::String("yes".to_string())),
    ]);
    assert!(crate::executor::mcp_command(Path::new("."), "csvx_pipeline_run", &pipeline).is_err());
}
