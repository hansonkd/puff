"""Webhook endpoint registration for Puff.

Webhooks are HTTP POST endpoints that receive events from external
services and route them to Python handler functions or named agents.

Usage:
    from puff.webhooks import webhook, get_webhook_handlers

    @webhook("/hooks/stripe", secret_env="STRIPE_WEBHOOK_SECRET")
    def handle_stripe(payload, headers):
        event_type = payload["type"]
        if event_type == "payment_intent.succeeded":
            process_payment(payload["data"]["object"])
        return {"status": "ok"}

    @webhook("/hooks/github", agent="dev-agent")
    def handle_github(payload, headers):
        # Payload is forwarded to the named agent
        return {"status": "ok"}

    # At startup, iterate registered handlers
    for path, config in get_webhook_handlers().items():
        print(f"Webhook: {path}")
"""
import hmac
import hashlib


_webhook_handlers = {}


class WebhookConfig:
    """Configuration for a single webhook endpoint."""

    def __init__(self, path, handler, secret_env=None, agent=None):
        self.path = path
        self.handler = handler
        self.secret_env = secret_env
        self.agent = agent


def webhook(path, secret_env=None, agent=None):
    """Register a webhook endpoint.

    Args:
        path: URL path (e.g. "/hooks/stripe").
        secret_env: Environment variable holding the HMAC secret for
            signature verification.  If set, the framework should call
            :func:`verify_signature` before invoking the handler.
        agent: Optional agent name.  When set, the payload is forwarded
            to the named Puff agent after the handler returns.
    """
    def decorator(func):
        config = WebhookConfig(
            path=path,
            handler=func,
            secret_env=secret_env,
            agent=agent,
        )
        _webhook_handlers[path] = config
        return func

    return decorator


def verify_signature(payload_bytes, signature, secret, algorithm="sha256"):
    """Verify a webhook HMAC signature (Stripe / GitHub style).

    Args:
        payload_bytes: Raw request body as bytes.
        signature: The signature string sent by the caller.
        secret: The shared secret (plain string).
        algorithm: Hash algorithm name (default ``sha256``).

    Returns:
        True if the signature matches, False otherwise.
    """
    expected = hmac.new(
        secret.encode(),
        payload_bytes,
        getattr(hashlib, algorithm),
    ).hexdigest()
    return hmac.compare_digest(expected, signature)


def get_webhook_handlers():
    """Return a dict of all registered webhook handlers, keyed by path."""
    return dict(_webhook_handlers)
