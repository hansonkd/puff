# Step 1: Setup your Puff project

Follow the instructions on how to setup a new poetry project and install Puff:

* [Using the CLI](https://github.com/hansonkd/puff/blob/master/book/CLI.md)

# Step 2: Add Django and Create a Project

```bash
poetry add django
poetry run django-admin startproject hello_world_django_app .
```

update `puff.toml`

```toml
wsgi = "hello_world_django_app.wsgi.application"
redis = true
postgres = true
django = true
```

Change `hello_world_django_app/wsgi.py` to serve static files:

```python
...

from django.core.wsgi import get_wsgi_application
from django.contrib.staticfiles.handlers import StaticFilesHandler

application = StaticFilesHandler(get_wsgi_application())
```

Update `hello_world_django_app/settings.py` so that Django uses the Puff Database and Redis pools:

```python
DATABASES = {
    'default': {
        'ENGINE': 'puff.contrib.django.postgres',
        'NAME': 'global',
    }
}

CACHES = {
    'default': {
        'BACKEND': 'puff.contrib.django.redis.PuffRedisCache',
        'LOCATION': 'redis://global',
    }
}
```

Run the server:

```bash
poetry run puff django migrate
poetry run puff runserver
```