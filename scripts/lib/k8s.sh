#!/usr/bin/env bash
# Helpers for the kind + local-registry deployment. Source this file.
#
# Defaults are overridable through the environment:
#   KIND_CLUSTER        kind cluster name (also CLUSTER)  (rpcstack-socialnet)
#   REGISTRY            local registry container name     (rpcstack-socialnet-registry)
#   REG_PORT            host port of the registry         (first free port from 5601)
#   MODE                baseline | policy                 (baseline)
#   SOCIALNET_NAMESPACE namespace (also NS); the whole stack is created in it
#                                                         (rpcstack-socialnet)
#   SOCIALNET_IMAGE_NAME repository name in the registry  (rpcstack-socialnet)
#   BUILDKIT_ADDR       e.g. tcp://127.0.0.1:1234: build and push with `buildctl`
#                       against that BuildKit daemon instead of `docker build` +
#                       `docker push` (uses a local buildctl, else $BUILDCTL_IMAGE)
CLUSTER=${KIND_CLUSTER:-${CLUSTER:-rpcstack-socialnet}}
REGISTRY=${REGISTRY:-rpcstack-socialnet-registry}
MODE=${MODE:-baseline}
NS=${SOCIALNET_NAMESPACE:-${NS:-rpcstack-socialnet}}
IMAGE_NAME=${SOCIALNET_IMAGE_NAME:-rpcstack-socialnet}

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

k8s_image() { echo "localhost:$(k8s_reg_port)/$IMAGE_NAME:$MODE"; }

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

# Build (or, with PREBUILT=1, reuse the local rpcstack-socialnet:$MODE image,
# e.g. one restored by `docker load` in CI) and push to the local registry.
k8s_build_push() {
  local image; image=$(k8s_image)
  if [ "${PREBUILT:-0}" = 1 ]; then
    echo "== tagging prebuilt rpcstack-socialnet:$MODE as $image"
    docker tag "rpcstack-socialnet:$MODE" "$image"
    docker push "$image"
  elif [ -n "${BUILDKIT_ADDR:-}" ]; then
    echo "== building $image (MODE=$MODE) with BuildKit at $BUILDKIT_ADDR"
    local opts=(--frontend dockerfile.v0 --opt "build-arg:MODE=$MODE"
      --output "type=image,name=$image,push=true,registry.insecure=true")
    if command -v buildctl > /dev/null; then
      buildctl --addr "$BUILDKIT_ADDR" build --local "context=$PWD" --local "dockerfile=$PWD" "${opts[@]}"
    else
      docker run --rm --network host --entrypoint buildctl -v "$PWD:/ctx:ro" \
        "${BUILDCTL_IMAGE:-moby/buildkit:v0.22.0}" --addr "$BUILDKIT_ADDR" build \
        --local context=/ctx --local dockerfile=/ctx "${opts[@]}"
    fi
  else
    echo "== building $image (MODE=$MODE)"
    docker build --build-arg "MODE=$MODE" -t "$image" .
    docker push "$image"
  fi
}

k8s_ctx() { echo "kind-$CLUSTER"; }
kc() { kubectl --context "$(k8s_ctx)" -n "$NS" "$@"; }

# Render k8s/base (baseline) or k8s/overlays/<mode> with kustomize, re-rooted in
# $NS (which also renames the Namespace object) and pointing the image at the
# registry's real host port and repository, and apply it. The ConfigMap reads
# config/ outside the kustomization, hence --load-restrictor.
k8s_deploy() {
  local overlay="$PWD/k8s/base" port render; port=$(k8s_reg_port)
  [ "$MODE" = baseline ] || overlay="$PWD/k8s/overlays/$MODE"
  render=$(mktemp -d)
  printf 'apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nnamespace: %s\nresources:\n  - %s\n' \
    "$NS" "$(realpath --relative-to="$render" "$overlay")" > "$render/kustomization.yaml"
  echo "== apply $overlay (namespace $NS)"
  kubectl --context "$(k8s_ctx)" kustomize --load-restrictor LoadRestrictionsNone "$render" \
    | sed "s#localhost:5001/rpcstack-socialnet#localhost:$port/$IMAGE_NAME#g" \
    | kubectl --context "$(k8s_ctx)" apply -f - >/dev/null
  rm -rf "$render"
  # Recreate the pods so a re-pushed tag is picked up.
  kc delete pod -l tier=app --wait=false >/dev/null
  kc wait --for=condition=Available deployment --all --timeout=600s
}

# k8s_run <name> <entrypoint args...>: run the image's entrypoint as a one-off
# pod and stream its output (the pod always re-pulls the tag).
k8s_run() {
  local name=$1 phase i; shift
  kc delete pod "$name" --ignore-not-found --wait >/dev/null
  kc run "$name" --restart=Never --image="$(k8s_image)" --image-pull-policy=Always -- "$@" >/dev/null
  # Wait for the pod to finish, then read its whole log (`kubectl run -i` can
  # lose the output of a pod that exits quickly).
  for i in $(seq 1 1800); do
    phase=$(kc get pod "$name" -o jsonpath='{.status.phase}' 2>/dev/null || true)
    case "$phase" in Succeeded|Failed) break;; esac
    sleep 2
  done
  kc logs "$name" 2>&1
  kc delete pod "$name" --wait=false >/dev/null 2>&1 || true
  [ "$phase" = Succeeded ]
}

# k8s_probe [probe args...]: run the stack probe as a one-off pod.
k8s_probe() { k8s_run probe probe "$@"; }

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
