wrk.method = "POST"
wrk.headers["Content-Type"] = "application/json"
wrk.body = os.getenv("GRAPHQL_BODY")

if wrk.body == nil then
    error("GRAPHQL_BODY env var must be set")
end
