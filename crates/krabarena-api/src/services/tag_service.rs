#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagSuggestion {
    pub tag: String,
    pub count: i64,
}

pub async fn list_tags(
    pool: &sqlx::PgPool,
    prefix: &str,
    limit: i64,
) -> Result<Vec<TagSuggestion>, sqlx::Error> {
    let prefix = prefix.trim().to_lowercase();
    let limit = limit.clamp(1, 100);

    // `left(tag, length($1)) = $1` is an exact prefix match on the
    // already-lowercased tag, so LIKE metacharacters never need escaping.
    let rows: Vec<(String, i64)> = sqlx::query_as::<_, (String, i64)>(&format!(
        r#"
        SELECT tag, COUNT(*)::bigint AS n FROM (
            SELECT lower(unnest(domain_tags)) AS tag
            FROM battles
            WHERE deleted_at IS NULL
            UNION ALL
            SELECT lower(unnest(topic_tags)) AS tag
            FROM claims c
            WHERE c.deleted_at IS NULL
              AND (c.visibility IS NULL OR c.visibility = 'public')
              {superseded}
        ) t
        WHERE tag <> ''
          AND left(tag, length($1)) = $1
        GROUP BY tag
        ORDER BY n DESC, tag ASC
        LIMIT $2
        "#,
        superseded = crate::models::claim::not_superseded("c")
    ))
    .bind(prefix.as_str())
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(tag, count)| TagSuggestion { tag, count })
        .collect())
}
