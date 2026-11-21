mod tests {
    use super::*;
    use crate::models::battle::BattleSolution;
    use crate::services::solution_names::CompiledVendor;

    /// Compiled vendor fixture for the refresh tests — the real map now
    /// lives in Neon (`vendor_logos`, migration 055); these tests pass a
    /// fixed seed covering the cases they assert.
    fn vendor_seed() -> Vec<CompiledVendor> {
        [
            (r"^snowflake", "snowflake.com"),
            (r"^brightdata\b|^bright data\b", "brightdata.com"),
            (r"^google$", "google.com"),
            (r"^native_python", "python.org"),
        ]
        .into_iter()
        .map(|(p, d)| CompiledVendor {
            regex: regex::Regex::new(&format!("(?i){p}")).unwrap(),
            domain: d.to_string(),
            // GCP-only: refresh writes the self-hosted asset URL.
            stored_url: Some(format!(
                "https://storage.googleapis.com/krabarena-artifacts/logos/{d}.png"
            )),
            brand_color: None,
        })
        .collect()
    }

    /// 0.14.5: backfill refresh — every entry the vendor map resolves
    /// gets `logo_url` + `website_url` rewritten to match the parser's
    /// current answer, INCLUDING editor-pinned overrides. Truly unknown
    /// vendors (parser returns None) are left untouched.
    ///
    /// Renamed from 0.13.2's `refresh_null_logos_fills_misses_and_preserves_overrides`
    /// — the "preserve overrides" guard is gone deliberately so the
    /// target-vendor-in-parens parser change propagates to every row.
    #[test]
    fn refresh_null_logos_rewrites_disagreements_including_overrides() {
        let mut merged = vec![
            // Null logo + vendor map hit → should be refreshed.
            BattleSolution {
                name: "Snowflake Medium".into(),
                name_normalized: "snowflake medium".into(),
                logo_url: None,
                website_url: None,
                brand_color: None,
            },
            // Editor-curated override that disagrees with the vendor
            // map → 0.14.5 now overwrites (was preserved in 0.13.2).
            BattleSolution {
                name: "Snowflake XS".into(),
                name_normalized: "snowflake xs".into(),
                logo_url: Some("https://custom.example/snow.png".into()),
                website_url: Some("https://example.com".into()),
                brand_color: None,
            },
            // The target-vendor-in-parens bug this PR is fixing:
            // entry was stored with the PROXY's logo; refresh must
            // overwrite to the target (google.com) now that the
            // parser resolves the parens content.
            BattleSolution {
                name: "BrightData (Google)".into(),
                name_normalized: "brightdata".into(),
                logo_url: Some(
                    "https://www.google.com/s2/favicons?domain=brightdata.com&sz=64".into(),
                ),
                website_url: Some("https://brightdata.com".into()),
                brand_color: None,
            },
            // Null logo + no vendor-map hit → stays null.
            BattleSolution {
                name: "MysteryVendor".into(),
                name_normalized: "mysteryvendor".into(),
                logo_url: None,
                website_url: None,
                brand_color: None,
            },
            // Null logo, regression for the `\b` bug (native_python_inverted_index).
            BattleSolution {
                name: "native_python_inverted_index".into(),
                name_normalized: "native_python_inverted_index".into(),
                logo_url: None,
                website_url: None,
                brand_color: None,
            },
        ];

        let refreshed = refresh_null_logos(&mut merged, &vendor_seed());
        assert_eq!(
            refreshed, 4,
            "Snowflake + Snowflake-override + BrightData-Google + native_python all refresh"
        );

        // Snowflake refreshed from the vendor map.
        assert_eq!(
            merged[0].logo_url.as_deref(),
            Some("https://storage.googleapis.com/krabarena-artifacts/logos/snowflake.com.png")
        );
        assert_eq!(
            merged[0].website_url.as_deref(),
            Some("https://snowflake.com")
        );

        // Editor override overwritten — 0.14.5 widening.
        assert_eq!(
            merged[1].logo_url.as_deref(),
            Some("https://storage.googleapis.com/krabarena-artifacts/logos/snowflake.com.png")
        );
        assert_eq!(
            merged[1].website_url.as_deref(),
            Some("https://snowflake.com")
        );

        // BrightData (Google) → flipped to google.com (the target).
        assert_eq!(
            merged[2].logo_url.as_deref(),
            Some("https://storage.googleapis.com/krabarena-artifacts/logos/google.com.png")
        );
        assert_eq!(merged[2].website_url.as_deref(), Some("https://google.com"));

        // Unknown vendor — still null (parser miss = leave alone).
        assert!(merged[3].logo_url.is_none());
        assert!(merged[3].website_url.is_none());

        // native_python_inverted_index regression — `\b` bug fix.
        assert_eq!(
            merged[4].logo_url.as_deref(),
            Some("https://storage.googleapis.com/krabarena-artifacts/logos/python.org.png")
        );
    }

    /// Logo cache-busting (0.43.3): when `refresh-assets` rewrites a
    /// vendor's `stored_url` to a new `?v=<hash>`, the battle backfill must
    /// see the old (unversioned or differently-versioned) URL as drift and
    /// rewrite it; once in sync, a second pass is a no-op.
    #[test]
    fn refresh_null_logos_picks_up_versioned_logo_urls() {
        let versioned =
            "https://storage.googleapis.com/krabarena-artifacts/logos/snowflake.com.png?v=0123456789";
        let mut seed = vendor_seed();
        seed[0].stored_url = Some(versioned.into());
        seed[0].brand_color = Some("#29b5e8".into());
        let mut merged = vec![BattleSolution {
            name: "Snowflake Medium".into(),
            name_normalized: "snowflake medium".into(),
            logo_url: Some(
                "https://storage.googleapis.com/krabarena-artifacts/logos/snowflake.com.png".into(),
            ),
            website_url: Some("https://snowflake.com".into()),
            brand_color: Some("#29b5e8".into()),
        }];
        assert_eq!(refresh_null_logos(&mut merged, &seed), 1);
        assert_eq!(merged[0].logo_url.as_deref(), Some(versioned));
        assert_eq!(refresh_null_logos(&mut merged, &seed), 0);
    }

    #[test]
    fn brand_color_change_parsing() {
        assert_eq!(
            parse_brand_color_change(None).unwrap(),
            BrandColorChange::Keep
        );
        assert_eq!(
            parse_brand_color_change(Some("#FF6700")).unwrap(),
            BrandColorChange::Pin("#ff6700".into())
        );
        assert_eq!(
            parse_brand_color_change(Some("auto")).unwrap(),
            BrandColorChange::Unpin
        );
        assert_eq!(
            parse_brand_color_change(Some("")).unwrap(),
            BrandColorChange::Unpin
        );
        for bad in ["ff6700", "#f60", "orange", "#ff67zz"] {
            assert!(
                matches!(
                    parse_brand_color_change(Some(bad)),
                    Err(ApiError::ValidationError(_))
                ),
                "{bad} must be rejected"
            );
        }
    }

    /// Idempotent: a second pass over an already-refreshed list is a no-op.
    #[test]
    fn refresh_null_logos_is_idempotent_on_second_pass() {
        let mut merged = vec![BattleSolution {
            name: "Snowflake Medium".into(),
            name_normalized: "snowflake medium".into(),
            logo_url: None,
            website_url: None,
            brand_color: None,
        }];
        assert_eq!(refresh_null_logos(&mut merged, &vendor_seed()), 1);
        // Second pass — logo_url is now Some, so nothing to refresh.
        assert_eq!(refresh_null_logos(&mut merged, &vendor_seed()), 0);
    }

    #[test]
    fn admin_claim_title_update_replaces_or_prepends_h1() {
        let next =
            set_markdown_title("# Old title\n\n## Summary\nOld summary.", "New title").unwrap();
        assert_eq!(next, "# New title\n\n## Summary\nOld summary.");

        let next = set_markdown_title("## Summary\nNo H1 here.", "Inserted title").unwrap();
        assert_eq!(next, "# Inserted title\n\n## Summary\nNo H1 here.");

        let err = set_markdown_title("# Old", "bad\ntitle").unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");
    }

    #[test]
    fn admin_claim_update_validates_structure_and_topic_tags() {
        validate_claim_structure_json(&serde_json::json!({ "schema_version": 1 })).unwrap();
        let err = validate_claim_structure_json(&serde_json::json!([1, 2, 3])).unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");

        let tags = normalize_topic_tags(vec![" rust ".into(), "cli".into()]).unwrap();
        assert_eq!(tags, vec!["rust", "cli"]);
        let err = normalize_topic_tags(vec!["ok".into(), " ".into()]).unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");
    }

    #[test]
    fn admin_reason_rejects_blank_but_keeps_none_compatible() {
        validate_admin_reason(None).unwrap();
        validate_recheck_reason(None).unwrap();

        let err = validate_admin_reason(Some("   ")).unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");

        let err = validate_recheck_reason(Some("   ")).unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");
    }

    #[test]
    fn admin_battle_status_filter_normalizes_legacy_values() {
        assert_eq!(
            normalize_admin_battle_status(Some("open")).unwrap(),
            Some("open".into())
        );
        assert_eq!(
            normalize_admin_battle_status(Some("contested")).unwrap(),
            Some("active".into())
        );
        assert_eq!(
            normalize_admin_battle_status(Some("holding")).unwrap(),
            Some("active".into())
        );
        let err = normalize_admin_battle_status(Some("paused")).unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");
    }

    #[test]
    fn admin_battle_list_item_reports_summary_and_solution_flags() {
        let row = AdminBattleRow {
            id: Uuid::nil(),
            author_id: Uuid::nil(),
            author_name: Some("Admin".into()),
            author_email: Some("admin@example.com".into()),
            entity_domain: Some("example.com".into()),
            parent_id: None,
            slug: Some("battle".into()),
            title: "Battle".into(),
            description: "Description".into(),
            domain_tags: vec!["rust".into()],
            status: "open".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            executive_summary: Some("Summary".into()),
            short_summary: Some("Short".into()),
            og_summary: None,
            proxy_spec: None,
            cover_image_url: None,
            cover_image_alt: None,
            solutions: serde_json::json!([{ "name": "Rust", "name_normalized": "rust" }]),
            deleted_at: None,
            claim_count: 1,
            live_claim_count: 1,
            deleted_claim_count: 0,
            total_confirmed: 2,
            total_refuted: 0,
            verification_rows: 2,
        };
        let item = admin_battle_list_item(row);
        assert!(item.has_summary);
        assert!(item.has_solutions);
        assert!(!item.missing_solutions);
        assert!(!item.stale_summary);
        assert_eq!(item.counts.live_claim_count, 1);
    }

    #[test]
    fn admin_list_pagination_and_order_by_validate_inputs() {
        let pagination = admin_list_pagination(Some(25), Some(50), Some(true)).unwrap();
        assert_eq!(pagination.limit, 25);
        assert_eq!(pagination.offset, 50);
        assert!(pagination.count_only);

        let order = admin_order_by(
            Some(" -updated_at, title "),
            ADMIN_BATTLE_SORT_FIELDS,
            ADMIN_BATTLE_DEFAULT_SORT,
            AdminSortTerm::asc("id", "b.id"),
        )
        .unwrap();
        assert_eq!(order, "b.updated_at DESC, b.title ASC, b.id ASC");

        let err = admin_order_by(
            Some("unknown"),
            ADMIN_BATTLE_SORT_FIELDS,
            ADMIN_BATTLE_DEFAULT_SORT,
            AdminSortTerm::asc("id", "b.id"),
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");

        let err = admin_list_pagination(Some(201), None, None).unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");
    }

    #[test]
    fn admin_user_filter_validation_accepts_new_filters() {
        let q = ListUsersQuery {
            email: Some("admin@example.com".into()),
            role: Some("admin".into()),
            entity_id: Some(Uuid::nil()),
            entity_domain: Some("example.com".into()),
            org_role: Some("member".into()),
            kind: Some("human".into()),
            suspended: Some(false),
            created_after: Some(Utc::now()),
            created_before: None,
            digest: None,
            limit: Some(25),
            offset: Some(0),
            count_only: Some(false),
            order_by: Some("-created_at,+user_id".into()),
        };
        validate_admin_user_filters(&q).unwrap();

        let q = ListUsersQuery {
            email: None,
            role: Some("owner".into()),
            entity_id: None,
            entity_domain: None,
            org_role: None,
            kind: None,
            suspended: None,
            created_after: None,
            created_before: None,
            digest: None,
            limit: None,
            offset: None,
            count_only: None,
            order_by: None,
        };
        let err = validate_admin_user_filters(&q).unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");

        let q = ListUsersQuery {
            email: None,
            role: None,
            entity_id: None,
            entity_domain: None,
            org_role: Some("owner".into()),
            kind: Some("robot".into()),
            suspended: None,
            created_after: None,
            created_before: None,
            digest: None,
            limit: None,
            offset: None,
            count_only: None,
            order_by: None,
        };
        let err = validate_admin_user_filters(&q).unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");
    }

    fn list_users_query_with_digest(digest: Option<&str>) -> ListUsersQuery {
        ListUsersQuery {
            email: None,
            role: None,
            entity_id: None,
            entity_domain: None,
            org_role: None,
            kind: None,
            suspended: None,
            created_after: None,
            created_before: None,
            digest: digest.map(str::to_string),
            limit: None,
            offset: None,
            count_only: None,
            order_by: None,
        }
    }

    /// `digest=eligible` is the only accepted value — anything else 400s
    /// so a typo (e.g. `digest=elligible`) doesn't silently no-op the
    /// unsubscribe filter and leak opted-out users into a send list.
    #[test]
    fn admin_user_digest_filter_only_accepts_eligible() {
        validate_admin_user_filters(&list_users_query_with_digest(None)).unwrap();
        validate_admin_user_filters(&list_users_query_with_digest(Some("eligible"))).unwrap();

        let err =
            validate_admin_user_filters(&list_users_query_with_digest(Some("all"))).unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");

        let err =
            validate_admin_user_filters(&list_users_query_with_digest(Some(""))).unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");
    }

    /// `digest_eligible_filter` feeds straight into the WHERE clause's
    /// `$10::bool IS NULL OR NOT EXISTS (...)` — `None` must mean "no
    /// filter" (param bound as SQL NULL) and `Some("eligible")` must
    /// mean "apply the NOT EXISTS exclusion" (bound as TRUE).
    #[test]
    fn admin_user_digest_eligible_filter_maps_to_bind_value() {
        assert_eq!(digest_eligible_filter(&list_users_query_with_digest(None)), None);
        assert_eq!(
            digest_eligible_filter(&list_users_query_with_digest(Some("eligible"))),
            Some(true)
        );
    }

    #[test]
    fn admin_user_response_includes_safety_signals() {
        let row = AdminUserRow {
            id: Uuid::nil(),
            entity_id: Uuid::nil(),
            display_name: "Admin User".into(),
            email: Some("admin@example.com".into()),
            entity_name: "Admin User".into(),
            entity_domain: "example.com".into(),
            role: "admin".into(),
            org_role: "member".into(),
            kind: "human".into(),
            suspended_until: Some(Utc::now() + chrono::Duration::days(1)),
            suspension_reason: Some("policy".into()),
            rate_limit_until: None,
            created_at: Utc::now(),
            claim_count: 3,
            battle_count: 1,
            last_activity_at: Some(Utc::now()),
        };
        let response = admin_user_response(row, Vec::new(), Vec::new(), Some(true));
        assert_eq!(response.role, "admin");
        assert_eq!(response.updated, Some(true));
        assert!(
            response
                .warnings
                .iter()
                .any(|item| item.contains("suspended"))
        );
        assert!(
            response
                .risk_signals
                .iter()
                .any(|item| item.contains("platform admin"))
        );
        assert!(
            response
                .risk_signals
                .iter()
                .any(|item| item.contains("public platform content"))
        );
        assert!(
            response
                .available_admin_actions
                .contains(&"lift_suspension".to_string())
        );
    }

    // -----------------------------------------------------------------
    // Claim recheck — `POST /v1/admin/claims/{id}/recheck` (since 0.15.2)
    //
    // The real handler does DB I/O (slug resolve + claim fetch + sub
    // count + dispatch), so we follow the same pattern as
    // `routes::orgs::tests::validate_set_permissions`: a pure
    // predicate mirrors the handler's pre-dispatch validation chain
    // (auth → claim-exists → reason-length) so the rejection branches
    // are exercised without a live Postgres. The dispatch path itself
    // is covered by `services::webhook_dispatch::tests` +
    // `tests/webhooks_e2e.rs`.
    //
    // Coverage matrix matches the spec:
    //   1. Non-admin caller → 403
    //   2. Claim not found → 404
    //   3. Admin + valid claim → 200, and dispatch_planned=true so
    //      callers can sanity-check the success branch sets up the
    //      dispatch envelope.
    // -----------------------------------------------------------------

    /// Pure-data analogue of `recheck_claim`'s validation chain.
    /// Returns `Ok(())` for "would dispatch" and an `ApiError` for
    /// every reject branch. The handler itself follows the same
    /// order: auth → reason-length → claim-exists.
    fn validate_recheck(
        caller_role: &str,
        reason: Option<&str>,
        claim_exists: bool,
    ) -> Result<(), ApiError> {
        // 1. Auth gate — `require_admin` only succeeds when role == "admin".
        if caller_role != "admin" {
            return Err(ApiError::Forbidden);
        }
        // 2. Body validation runs before the DB lookup so a malformed
        //    reason 400s before we touch Postgres.
        validate_recheck_reason(reason)?;
        // 3. Claim lookup — `slug_resolver::resolve_claim` returns
        //    `ApiError::ClaimNotFound` (→ 404) on miss.
        if !claim_exists {
            return Err(ApiError::ClaimNotFound);
        }
        Ok(())
    }

    /// 1. Non-admin caller → 403 even with a valid claim and clean reason.
    #[test]
    fn claim_recheck_rejects_non_admin() {
        let err = validate_recheck("user", Some("transient failure"), true)
            .expect_err("non-admin must not be allowed");
        assert!(matches!(err, ApiError::Forbidden), "{err:?}");

        let err = validate_recheck("editor", None, true).expect_err("editor is not platform admin");
        assert!(matches!(err, ApiError::Forbidden), "{err:?}");

        let err =
            validate_recheck("moderator", None, true).expect_err("moderator is not platform admin");
        assert!(matches!(err, ApiError::Forbidden), "{err:?}");
    }

    /// 2. Claim not found → 404 (not 200 with subscriber_count=0).
    #[test]
    fn claim_recheck_rejects_missing_claim() {
        let err = validate_recheck("admin", None, false).expect_err("missing claim must 404");
        assert!(matches!(err, ApiError::ClaimNotFound), "{err:?}");
    }

    /// 3. Admin + valid claim → success branch. The pure predicate
    /// returns `Ok(())` which the handler treats as "build payload,
    /// count subs, dispatch, return 200". We can't assert
    /// `dispatch_event` was actually called without a DB pool (the
    /// dispatcher's spawn-internal task does the SQL itself), so we
    /// assert the gate the handler waits on before invoking dispatch.
    #[test]
    fn claim_recheck_admin_with_valid_claim_succeeds() {
        assert!(validate_recheck("admin", None, true).is_ok());
        assert!(validate_recheck("admin", Some("rerun after edit"), true).is_ok());
    }

    /// Reason length cap — `MAX_RECHECK_REASON_LEN` = 500. A 501-char
    /// note → 400, a 500-char note → 200.
    #[test]
    fn claim_recheck_rejects_overlong_reason() {
        let max = "x".repeat(MAX_RECHECK_REASON_LEN);
        assert!(validate_recheck("admin", Some(&max), true).is_ok());

        let over = "x".repeat(MAX_RECHECK_REASON_LEN + 1);
        let err =
            validate_recheck("admin", Some(&over), true).expect_err("501-char reason must 400");
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");
    }

    // -----------------------------------------------------------------
    // Claim presentation regen — `POST
    // /v1/admin/claims/{id}/regenerate-presentation` (viral-claim-
    // presentation backfill task 7)
    //
    // The real handler does DB I/O (slug resolve + claim fetch + a
    // Gemini call + persist), so — same rationale as `validate_recheck`
    // above — a pure predicate mirrors the handler's pre-Gemini-call
    // validation chain (auth → claim-exists) without a live Postgres or
    // a Gemini stub. The generator itself
    // (`services::presentation_gen::generate_presentation_for_claim`)
    // is covered by its own module tests (Section F extraction, prompt
    // assembly); the sanitize step is covered by
    // `services::presentation_check::tests`.
    //
    // Coverage matrix:
    //   1. Non-admin caller → 403
    //   2. Claim not found → 404
    //   3. Admin + valid claim → would proceed to the Gemini call
    // -----------------------------------------------------------------

    /// Pure-data analogue of `regenerate_presentation`'s pre-Gemini-call
    /// validation chain. Returns `Ok(())` for "would call the generator"
    /// and an `ApiError` for every reject branch, in the same order the
    /// handler checks them: auth → claim-exists.
    fn validate_regenerate_presentation(
        caller_role: &str,
        claim_exists: bool,
    ) -> Result<(), ApiError> {
        if caller_role != "admin" {
            return Err(ApiError::Forbidden);
        }
        if !claim_exists {
            return Err(ApiError::ClaimNotFound);
        }
        Ok(())
    }

    /// 1. Non-admin caller → 403 even with a valid claim.
    #[test]
    fn regenerate_presentation_rejects_non_admin() {
        let err = validate_regenerate_presentation("user", true)
            .expect_err("non-admin must not be allowed");
        assert!(matches!(err, ApiError::Forbidden), "{err:?}");

        let err = validate_regenerate_presentation("editor", true)
            .expect_err("editor is not platform admin");
        assert!(matches!(err, ApiError::Forbidden), "{err:?}");

        let err = validate_regenerate_presentation("moderator", true)
            .expect_err("moderator is not platform admin");
        assert!(matches!(err, ApiError::Forbidden), "{err:?}");
    }

    /// 2. Claim not found → 404 (not a 500 from a downstream Gemini call
    ///    that should never fire).
    #[test]
    fn regenerate_presentation_rejects_missing_claim() {
        let err = validate_regenerate_presentation("admin", false)
            .expect_err("missing claim must 404");
        assert!(matches!(err, ApiError::ClaimNotFound), "{err:?}");
    }

    /// 3. Admin + valid claim → success branch (would proceed to call the
    ///    generator + persist + warm the OG card).
    #[test]
    fn regenerate_presentation_admin_with_valid_claim_succeeds() {
        assert!(validate_regenerate_presentation("admin", true).is_ok());
    }

    // -----------------------------------------------------------------
    // Soft-delete / restore auth gate (since 0.21.0)
    //
    // The four soft-delete handlers (battle/claim × delete/restore) all
    // do the same thing structurally: `require_admin` → slug-resolve →
    // single UPDATE. The DB-touching behaviour (filtering, idempotency,
    // restore-finds-deleted-row) is covered by the
    // `summary_integrity_e2e` integration test. Here we assert the
    // ONLY app-side gate they have — admin-only — mirrors
    // `require_admin`'s contract (role must be exactly "admin"; editor
    // / moderator / user are all rejected). This pure predicate stands
    // in for `require_admin` so we don't need a signed JWT in a unit
    // test.
    // -----------------------------------------------------------------

    fn soft_delete_allowed(caller_role: &str) -> Result<(), ApiError> {
        if caller_role != "admin" {
            return Err(ApiError::Forbidden);
        }
        Ok(())
    }

    #[test]
    fn soft_delete_is_admin_only() {
        assert!(soft_delete_allowed("admin").is_ok());
        for role in ["editor", "moderator", "user", ""] {
            let err =
                soft_delete_allowed(role).expect_err("non-admin must not soft-delete / restore");
            assert!(matches!(err, ApiError::Forbidden), "{role}: {err:?}");
        }
    }

    // -----------------------------------------------------------------
    // GET /v1/admin/voters (migration 068) — digest audience listing
    // for voters, mirroring users?digest=eligible.
    // -----------------------------------------------------------------

    #[test]
    fn admin_voters_limit_defaults_and_caps() {
        assert_eq!(voters_limit(None).unwrap(), ADMIN_VOTERS_DEFAULT_LIMIT);
        assert_eq!(voters_limit(Some(1)).unwrap(), 1);
        assert_eq!(
            voters_limit(Some(ADMIN_VOTERS_MAX_LIMIT)).unwrap(),
            ADMIN_VOTERS_MAX_LIMIT
        );

        let err = voters_limit(Some(0)).unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");

        let err = voters_limit(Some(ADMIN_VOTERS_MAX_LIMIT + 1)).unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");
    }

    /// `digest=eligible` is the only accepted value — same rationale
    /// as the `users` endpoint: a typo must not silently no-op the
    /// opt-out filter.
    #[test]
    fn admin_voters_digest_filter_only_accepts_eligible() {
        validate_voters_digest_filter(&None).unwrap();
        validate_voters_digest_filter(&Some("eligible".to_string())).unwrap();

        let err = validate_voters_digest_filter(&Some("all".to_string())).unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");
    }

    #[test]
    fn admin_voters_digest_eligible_filter_maps_to_bind_value() {
        assert_eq!(voters_digest_eligible_filter(&None), None);
        assert_eq!(
            voters_digest_eligible_filter(&Some("eligible".to_string())),
            Some(true)
        );
    }
}
