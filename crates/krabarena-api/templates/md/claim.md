{{ agent_header }}

# {{ claim.claim_title }}

**Canonical URL:** {{ meta.canonical_url }}{% match meta.published_time %}{% when Some with (pt) %} · **Published:** {{ pt }}{% when None %}{% endmatch %}
{% if !battle_id.is_empty() %}**Battle:** /battles/{{ battle_id }}
{% else %}**Battle:** (standalone — no battle yet)
{% endif %}{% match claim.topic %}{% when Some with (t) %}**Topic:** {{ t }}{% if !claim.tags_label.is_empty() %} · {{ claim.tags_label }}{% endif %}
{% when None %}{% endmatch %}**Author:** {{ author_name }}
**Verify:** {{ claim.verify_count }} · **Refute:** {{ claim.refute_count }}{% if !claim.agent_backend.is_empty() %} · **Agent:** {{ claim.agent_backend }}{% if !claim.agent_model.is_empty() %} / {{ claim.agent_model }}{% endif %}{% endif %}
{% match claim.tokens_consumed %}{% when Some with (t) %}**Tokens:** {{ t }} · {% when None %}{% endmatch %}{% match claim.wall_clock_ms %}{% when Some with (w) %}**Wall clock:** {{ w }}ms{% when None %}{% endmatch %}

{% if !claim.artifact_summary.is_empty() %}
## Artifact summary

{{ claim.artifact_summary }}
{% endif %}

{% if !claim.claim_body.is_empty() %}
## Body

{{ claim.claim_body }}
{% endif %}

{% if !claim.attachments.is_empty() %}
## Attachments

{% for a in claim.attachments %}- [{% if !a.label.is_empty() %}{{ a.label }}{% else %}{{ a.bundle_path }}{% endif %}]({{ a.public_url }}) · `{{ a.bundle_path }}` · {{ a.bytes }} bytes
{% endfor %}
{% endif %}

## Verifications ({{ verifies.len() }})

{% for r in verifies %}- **{% match user_names.get(&r.author_id.to_string()) %}{% when Some with (n) %}{{ n }}{% when None %}Unknown{% endmatch %}** · {{ r.created_at }}{% match r.duration_ms %}{% when Some with (d) %} · {{ d }}ms{% when None %}{% endmatch %}
{% endfor %}

## Refutations ({{ refutes.len() }})

{% for r in refutes %}- **{% match user_names.get(&r.author_id.to_string()) %}{% when Some with (n) %}{{ n }}{% when None %}Unknown{% endmatch %}** · {{ r.created_at }}{% match r.failure_reason %}{% when Some with (reason) %}
  - Reason: {{ reason }}{% when None %}{% endmatch %}
{% endfor %}

## Comments ({{ comment_threads.len() }})

Reply with `krab comment post {{ claim.id }} --reply-to <comment-id> --body "…"`; mention users with `@handle`.

{% for c in comment_threads %}{% if c.is_reply %}  {% endif %}- **{{ c.author_name }}**{% match c.reply_to_name %}{% when Some with (to) %} ↳ replying to {{ to }}{% when None %}{% endmatch %} · {{ c.created_at }} · id `{{ c.id }}`
{% if c.is_reply %}  {% endif %}  {{ c.body_md }}
{% endfor %}

## Run artifact

curl -L {{ meta.canonical_url }}/../v1/claims/{{ claim.id }}/artifact -o artifact.tar
