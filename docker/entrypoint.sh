#!/bin/sh
# Select a socialnet binary by service name. Services are configured through
# environment variables (see docker-compose.yaml / k8s/base), not a config file.
#
#   <service>                 post-storage-service user-timeline-service
#                             home-timeline-service write-home-timeline-service
#                             user-service social-graph-service
#                             unique-id-service media-service text-service
#                             user-mention-service url-shorten-service
#                             register-user-service frontend-service
#                             compose-post-service
#   client-bench [args...]    wait for the frontend, then run the load generator
#                             driven by $SOCIALNET_GEN_CONFIG, traces to
#                             $SOCIALNET_OUTPUT_PATH
#   probe [args...]           policy-liveness probe (socialnet_policy_probe)
#   mode                      print the policy mode baked into this image
set -eu

export RUST_LOG="${RUST_LOG:-${LOG_LEVEL:-info}}"
svc="${1:-}"
[ $# -gt 0 ] && shift

bin=""
case "$svc" in
  post-storage-service) bin=post_storage_server ;;
  user-timeline-service) bin=user_timeline_server ;;
  home-timeline-service) bin=home_timeline_server ;;
  write-home-timeline-service) bin=write_home_timeline_server ;;
  user-service) bin=user_service ;;
  social-graph-service) bin=social_graph_server ;;
  unique-id-service) bin=unique_id_service ;;
  media-service) bin=media_service ;;
  text-service) bin=textservice_server ;;
  user-mention-service) bin=usermention_server ;;
  url-shorten-service) bin=url_shorten_server ;;
  register-user-service) bin=register_user_server ;;
  frontend-service) bin=frontend_server ;;
  compose-post-service) bin=compose_post_server ;;
esac

if [ -n "$bin" ]; then
  echo "socialnet: starting $bin ($(cat /etc/socialnet-mode))" >&2
  exec "/usr/local/bin/$bin" "$@"
fi

case "$svc" in
  client-bench)
    /usr/local/bin/socialnet_policy_probe --expect none \
      --frontend "${SOCIALNET_FRONTEND:-http://frontend-service:8080}" \
      --wait-secs "${SOCIALNET_WAIT_SECS:-300}" --control 0
    exec /usr/local/bin/socialnet_client_bench \
      --gen-config "${SOCIALNET_GEN_CONFIG:-/etc/socialnet/gen_config.json}" \
      --output-path "${SOCIALNET_OUTPUT_PATH:-/out}" --save-logs "$@"
    ;;
  probe)
    exec /usr/local/bin/socialnet_policy_probe \
      --frontend "${SOCIALNET_FRONTEND:-http://frontend-service:8080}" "$@"
    ;;
  mode)
    cat /etc/socialnet-mode
    ;;
  *)
    echo "usage: socialnet-entrypoint <service>|client-bench|probe|mode [args...]" >&2
    exit 2
    ;;
esac
