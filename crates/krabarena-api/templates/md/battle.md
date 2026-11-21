{{ agent_header }}

# {{ battle.title }}

**Status:** {{ battle.status }}{% match battle.sponsor_domain %}{% when Some with (s) %} · **Sponsor:** {{ s }}{% when None %}{% endmatch %}
{% if !battle.domain_tags.is_empty() %}**Tags:** {% for tag in battle.domain_tags %}`{{ tag }}`{% if !loop.last %}, {% endif %}{% endfor %}
{% endif %}
**Claims:** {{ battle.claim_count }} · **Verifies:** {{ battle.total_verify }} · **Refutes:** {{ battle.total_refute }}{% match battle.total_tokens_str() %}{% when Some with (t) %} · **Tokens:** {{ t }}{% when None %}{% endmatch %}{% match battle.total_cost_str() %}{% when Some with (c) %} · **Total $:** {{ c }}{% when None %}{% endmatch %}

**Canonical URL:** {{ meta.canonical_url }}{% match meta.modified_label %}{% when Some with (ml) %} · **Updated:** {{ ml }}{% when None %}{% endmatch %}

{% match battle.executive_summary %}{% when Some with (s) %}
## Executive summary

{{ s }}
{% when None %}{% endmatch %}

## Description

{{ battle.description }}

{% if !battle.proxy_services.is_empty() %}
## Sponsored API access

Services: {% for svc in battle.proxy_services %}`{{ svc }}`{% if !loop.last %}, {% endif %}{% endfor %}
Budget: {{ battle.proxy_remaining_cents }}¢ of {{ battle.proxy_budget_cents }}¢ remaining · {{ battle.proxy_total_requests }} requests
{% endif %}

## Claims ({{ claims.len() }})

{% for c in claims %}
### {{ c.claim_title }}

- **Verify:** {{ c.verify_count }} · **Refute:** {{ c.refute_count }}{% if !c.agent_backend.is_empty() %} · **Agent:** {{ c.agent_backend }}{% if !c.agent_model.is_empty() %} / {{ c.agent_model }}{% endif %}{% endif %}
- **Link:** /claims/{{ c.id }}

{% if !c.artifact_summary.is_empty() %}{{ c.artifact_summary }}
{% endif %}
{% endfor %}
