#!/usr/bin/env bash
# Helpers for the kind + local-registry deployment. Source this file.
#
# Defaults are overridable through the environment:
#   CLUSTER   kind cluster name              (rpcstack-socialnet)
#   REGISTRY  local registry container name  (rpcstack-socialnet-registry)
#   REG_PORT  host port of the registry      (first free port from 5601)
#   MODE      baseline | masa                (baseline)
CLUSTER=${CLUSTER:-rpcstack-socialnet}
REGISTRY=${REGISTRY:-rpcstack-socialnet-registry}
MODE=${MODE:-baseline}
NS=${NS:-socialnet}

k8s_free_port() {
  local p
  for p in $(seq 5601 5699); do
    if ! (exec 3<>"/dev/tcp/127.0.0.1/$p") 2>/dev/null; then echo "$p"; return; fi
  done
  echo "no free port for the registry" >&2; return 1
}

# Port of an existing registry container, else a free one.
k8s_reg_port() {
  if [ -n "${REG_PORT:-}" ]; then echo "$REG_PORT"; return; fi
  local existing
  existing=$(docker port "$REGISTRY" 5000/tcp 2>/dev/null | head -n 1 | sed 's/.*://') || true
  if [ -n "$existing" ]; then echo "$existing"; else k8s_free_port; fi
}

k8s_image() { echo "localhost:$(k8s_reg_port)/rpcstack-socialnet:$MODE"; }

k8s_cluster_up() {
  local port; port=$(k8s_reg_port)
  if ! docker inspect "$REGISTRY" >/dev/null 2>&1; then
    echo "== starting registry $REGISTRY on 127.0.0.1:$port"
    docker run -d --restart=always -p "127.0.0.1:$port:5000" --name "$REGISTRY" registry:2 >/dev/null
  fi
  if ! kind get clusters 2>/dev/null | grep -qx "$CLUSTER"; then
    echo "== creating kind cluster $CLUSTER"
    cat <<YAML | kind create cluster --name "$CLUSTER" --wait 120s --config=-
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
containerdConfigPatches:
- |-
  [plugins."io.containerd.grpc.v1.cri".registry]
    config_path = "/etc/containerd/certs.d"
YAML
  fi
  # Nodes pull localhost:<port>/... from the registry container over the kind network.
  local node dir="/etc/containerd/certs.d/localhost:$port"
  for node in $(kind get nodes --name "$CLUSTER"); do
    docker exec "$node" mkdir -p "$dir"
    printf '[host."http://%s:5000"]\n' "$REGISTRY" | docker exec -i "$node" sh -c "cat > $dir/hosts.toml"
  done
  if [ "$(docker inspect -f '{{json .NetworkSettings.Networks.kind}}' "$REGISTRY")" = null ]; then
    docker network connect kind "$REGISTRY"
  fi
}

k8s_build_push() {
  local image; image=$(k8s_image)
  echo "== building $image (MODE=$MODE)"
  docker build --build-arg "MODE=$MODE" -t "$image" .
  docker push "$image"
}

k8s_ctx() { echo "kind-$CLUSTER"; }
kc() { kubectl --context "$(k8s_ctx)" -n "$NS" "$@"; }

k8s_deploy() {
  local image; image=$(k8s_image)
  kubectl --context "$(k8s_ctx)" create namespace "$NS" --dry-run=client -o yaml \
    | kubectl --context "$(k8s_ctx)" apply -f - >/dev/null
  sed "s#__IMAGE__#$image#g" k8s/socialnet.yaml | kc apply -f -
  kc wait --for=condition=Available deployment --all --timeout=600s
}

# k8s_probe <expect> [probe args...]: run the policy probe as a one-off pod.
k8s_probe() {
  local expect=$1; shift
  kc delete pod probe --ignore-not-found >/dev/null
  kc run probe --rm -i --restart=Never --image="$(k8s_image)" --image-pull-policy=IfNotPresent \
    --command -- socialnet_policy_probe --expect "$expect" --frontend http://frontend-service:8080 "$@"
}

# Delete the load generator Job and wait until its pod is really gone, so the
# next run cannot pick up the previous pod or its /out/DONE marker.
k8s_delete_loadgen() {
  kc delete job loadgen --ignore-not-found --wait >/dev/null
  kc wait --for=delete pod -l job-name=loadgen --timeout=180s >/dev/null 2>&1 || true
}

# k8s_loadgen <gen-config file name under config/> [outdir]: run the load
# generator Job; with an outdir, wait for it and copy the traces there.
k8s_loadgen() {
  local cfg=$1 out=${2:-}
  k8s_delete_loadgen
  kc create configmap loadgen-config --from-file=config --dry-run=client -o yaml | kc apply -f - >/dev/null
  sed "s#__IMAGE__#$(k8s_image)#g; s#__GEN_CONFIG__#$cfg#g" k8s/loadgen-job.yaml | kc apply -f -
  [ -n "$out" ] || return 0
  local pod i
  for i in $(seq 1 60); do
    pod=$(kc get pod -l job-name=loadgen -o jsonpath='{.items[0].metadata.name}' 2>/dev/null) && [ -n "$pod" ] && break
    sleep 2
  done
  # The Job writes /out/DONE (inside the container) when the run completes.
  for i in $(seq 1 360); do
    if kc exec "$pod" -- test -f /out/DONE 2>/dev/null; then break; fi
    if [ "$(kc get pod "$pod" -o jsonpath='{.status.phase}')" = Failed ]; then
      kc logs "$pod" | tail -n 30; return 1
    fi
    sleep 5
  done
  mkdir -p "$out"; rm -f "$out"/r*_*.csv
  kc exec "$pod" -- tar -C /out -cf - . | tar -C "$out" -xf -
  k8s_delete_loadgen
}

k8s_down() {
  kind delete cluster --name "$CLUSTER" || true
  docker rm -f "$REGISTRY" >/dev/null 2>&1 || true
}
