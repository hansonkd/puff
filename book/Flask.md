# Step 1: Setup your Puff project

Follow the instructions on how to setup a new poetry project and install Puff:

* [Using the CLI](https://github.com/hansonkd/puff/blob/master/book/CLI.md)

# Step 2: Add a Flask app

Install Flask to your your project:

```
poetry add flask
```

Add your flask application in a new file `my_puff_project/flask.py`

```python
from flask import Flask

app = Flask(__name__)

@app.route("/")
def hello_world():
    return "<p>Hello, World!</p>"
```

Update `puff.toml` with the following lines

```toml
wsgi = "my_puff_project.flask.app"
```

Run the server:

```bash
poetry run puff runserver
```

# Step 3: Add GraphQL

Create a new file called `my_puff_project/graphql.py`

```python
from typing import Optional, Iterable
from dataclasses import dataclass
from puff.pubsub import global_pubsub as pubsub


@dataclass
class Query:
    @classmethod
    def new_connection_id(cls, context, /) -> str:
        # Get a new connection identifier for pubsub.
        return pubsub.new_connection_id()


@dataclass
class Mutation:
    @classmethod
    def send_message_to_channel(cls, context, /, connection_id: str, channel: str, message: str) -> bool:
        print(context.auth_token) #  Authorization bearer token passed in the context
        return pubsub.publish_as(connection_id, channel, message)


@dataclass
class MessageObject:
    message_text: str
    from_connection_id: str
    num_processed: int


@dataclass
class Subscription:
    @classmethod
    def read_messages_from_channel(cls, context, /, channel: str, connection_id: Optional[str] = None) -> Iterable[MessageObject]:
        if connection_id is not None:
            conn = pubsub.connection_with_id(connection_id)
        else:
            conn = pubsub.connection()
        conn.subscribe(channel)
        num_processed = 0
        while msg := conn.receive():
            from_connection_id = msg.from_connection_id
            # Filter out messages from yourself.
            if connection_id != from_connection_id:
                yield MessageObject(message_text=msg.text, from_connection_id=from_connection_id, num_processed=num_processed)
                num_processed += 1


@dataclass
class Schema:
    query: Query
    mutation: Mutation
    subscription: Subscription
```

Update `puff.toml` with the following lines

```toml
pubsub = true
graphql = "my_puff_project.graphql.Schema"
graphql_url = "/graphql/"
graphql_subscription_url = "/subscriptions/"
```

Run the server and go to `http://localhost:7777/graphql/`

# Step 4: Add Distributed Tasks

Update `puff.toml` with the following lines

```toml
task_queue = true
```

Create a new file called `my_puff_project/tasks.py`

```python
def my_task(payload):
    print(f"Inside `my_task` with payload: {payload}")
    return {"from_task": payload}
```

Change your `Mutation` class in `graphql.py` to call the function through the queue:

```python
from puff.task_queue import global_task_queue as tq
from my_puff_project.tasks import my_task

...

@dataclass
class Mutation:
    @classmethod
    def send_message_to_channel(cls, context, /, connection_id: str, channel: str, message: str) -> bool:
        print(context.auth_token) #  Authorization bearer token passed in the context
        tq.add_task(my_task, {"auth_token": context.auth_token, "connection_id": connection_id, "channel": channel, "message": message})
        return pubsub.publish_as(connection_id, channel, message)
```

Run a worker server by using wait_forever in another tab:

```bash
poetry run puff wait_forever
```

Now run the server and calling the mutation will trigger tasks.

```bash
poetry run puff runserver
```
