import pytest
from puff.graphql import global_graphql as graphql
from puff.graphql import named_client


alt_client = named_client("alt")


TEST_QUERY = """
query TestGQL {
    hello_world
}
"""


TEST_QUERY_INPUT = """
query TestGQL($my_input: Int) {
    hello_world(my_input: $my_input)
}
"""


def test_gql_query():
    resp = graphql.query(TEST_QUERY, {})
    assert "errors" not in resp
    assert "data" in resp
    assert "hello_world" in resp["data"]
    assert resp["data"]["hello_world"] == "hello: None"


def test_gql_query_input():
    resp = graphql.query(TEST_QUERY_INPUT, {"my_input": 12})
    assert "errors" not in resp
    assert "data" in resp
    assert "hello_world" in resp["data"]
    assert resp["data"]["hello_world"] == "hello: 12"


def test_gql_query_wrong_input():
    with pytest.raises(Exception):
        graphql.query(TEST_QUERY_INPUT, {"my_input": True})


TEST_SUBSCRIPTION = """
subscription TestGQL($num: Int) {
    read_some_objects(num: $num) {
        field1
    }
}
"""

TEST_SUBSCRIPTION_ASYNC = """
subscription TestGQL($num: Int) {
    async_read_some_objects(num: $num) {
        field1
    }
}
"""

SUB_TYPES = {
    "async_": TEST_SUBSCRIPTION_ASYNC,
    "": TEST_SUBSCRIPTION,
}


@pytest.mark.parametrize("query", ["", "async_"])
def test_gql_subscription(query):
    name = f"{query}read_some_objects"
    subscription = graphql.subscribe(SUB_TYPES[query], {"num": 2})
    resp = subscription.receive()
    assert resp is not None
    assert resp[0] == name
    resp = resp[1]
    assert "errors" not in resp
    assert "data" in resp
    assert "field1" in resp["data"]
    assert resp["data"]["field1"] == 0

    resp = subscription.receive()
    assert resp is not None
    assert resp[0] == name
    resp = resp[1]
    assert "errors" not in resp
    assert "data" in resp
    assert "field1" in resp["data"]
    assert resp["data"]["field1"] == 1

    resp = subscription.receive()
    assert resp is None


TEST_QUERY_ALT = """
query TestGQL {
    alt_hello_world
}
"""


def test_gql_query_alt_client():
    resp = alt_client.query(TEST_QUERY_ALT, {})
    assert "errors" not in resp
    assert "data" in resp
    assert "alt_hello_world" in resp["data"]
    assert resp["data"]["alt_hello_world"] == "hello from alternate"


TEST_QUERY_LIST = """
query TestGQL {
    hello_world_object {
        field1
    }
}
"""


def test_gql_query_list():
    resp = graphql.query(TEST_QUERY_LIST, {})
    assert "errors" not in resp
    assert "data" in resp
    assert "hello_world_object" in resp["data"]


TEST_QUERY_LIST_NESTED = """
query TestGQL {
    hello_world_objects {
        field1
        hello_world2 {
            field2
            hello_world2 {
                hello_world2 {
                    example_self
                    field2
                    hello_world_query {
                        parent_id
                    }
                }
            }
        }
        hello_world_query {
            parent_id
        }
    }
}
"""


def test_gql_query_list_nested():
    resp = graphql.query(TEST_QUERY_LIST_NESTED, {})
    assert "errors" not in resp
    assert "data" in resp
    assert "hello_world_objects" in resp["data"]
    for obj in resp["data"]["hello_world_objects"]:
        assert "hello_world2" in obj
        assert "field2" in obj["hello_world2"]
        assert "hello_world_query" in obj
        for sub_obj in obj["hello_world_query"]:
            assert "parent_id" in sub_obj
            assert sub_obj["parent_id"] == obj["field1"]
        assert "hello_world2" in obj["hello_world2"]
        assert "field2" in obj["hello_world2"]
        assert "hello_world2" in obj["hello_world2"]["hello_world2"]
        assert "field2" not in obj["hello_world2"]["hello_world2"]
        assert "hello_world2" in obj["hello_world2"]["hello_world2"]
        assert "field2" in obj["hello_world2"]["hello_world2"]["hello_world2"]
        assert "example_self" in obj["hello_world2"]["hello_world2"]["hello_world2"]
        assert obj["hello_world2"]["hello_world2"]["hello_world2"]["example_self"] == "None: hello: None"
        assert "hello_world_query" in obj["hello_world2"]["hello_world2"]["hello_world2"]
        for sub_obj in obj["hello_world2"]["hello_world2"]["hello_world2"]["hello_world_query"]:
            assert "parent_id" in sub_obj
            assert sub_obj["parent_id"] == 42


TEST_QUERY_LIST_NESTED_ERROR = """
query TestGQL {
    hello_world_objects {
        hello_world2 {
            field1
        }
    }
}
"""


def test_gql_query_list_nested_error():
    resp = graphql.query(TEST_QUERY_LIST_NESTED_ERROR, {})
    assert "errors" in resp


TEST_QUERY_LIST_SELF = """
query TestGQL {
    hello_world_objects {
        field1
        field2
        example_self
    }
}
"""


def test_gql_query_list_self():
    resp = graphql.query(TEST_QUERY_LIST_SELF, {})
    assert "errors" not in resp
    assert "data" in resp
    for obj in resp["data"]["hello_world_objects"]:
        assert obj["example_self"] == f"{obj['field1']}: {obj['field2']}"
