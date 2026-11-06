package oracle

import (
	"fmt"
	"io"
	"sort"

	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/check"

	"rpc-policy-k8s/evaluator/internal/social"
)

// Stats writes what a history exercised: the calls by operation and outcome, how many posts the
// timelines served and how deep, and how many follow changes and mentions there were. It is how
// the README's coverage numbers are made (cmd/oracle stats).
func Stats(h *check.History, w io.Writer) error {
	data := social.Default()
	if h.Dataset != "" {
		var err error
		if data, err = social.ReadDataset(h.Dataset); err != nil {
			return err
		}
	}
	f, err := Collect(data, phasesOf(h))
	if err != nil {
		return err
	}
	count := map[string]int{}
	var served, mentioned, edges, deep, nonEmpty, home int
	err = calls(phasesOf(h), func(ac *acall) error {
		key := ac.c.Op
		switch {
		case ac.unknown:
			key += " unknown"
		case ac.a.Invalid != "":
			key += " (" + ac.a.Invalid + ")"
		}
		count[fmt.Sprintf("phase %d: %s", ac.c.Phase, key)]++
		count["all: "+key]++
		if ac.unknown || !ac.valid() {
			return nil
		}
		switch ac.a.Kind {
		case social.KindFollow, social.KindUnfollow:
			edges++
		case social.KindCompose:
			if ac.a.Mention != 0 {
				mentioned++
			}
		case social.KindRead:
			if posts, ok := ac.page(); ok {
				if ac.a.Home {
					home++
					if len(posts) > 0 {
						nonEmpty++
					}
				}
				served += len(posts)
				if ac.a.Start > 0 && len(posts) > 0 {
					deep++
				}
			}
		}
		return nil
	})
	if err != nil {
		return err
	}
	keys := make([]string, 0, len(count))
	for k := range count {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		fmt.Fprintf(w, "%-50s %6d\n", k, count[k])
	}
	seen := 0
	for i := range f.posts {
		if f.posts[i].served != nil {
			seen++
		}
	}
	fmt.Fprintf(w, "composes: %d, served at least once: %d, with a mention: %d\n", len(f.posts), seen, mentioned)
	fmt.Fprintf(w, "follow changes answered: %d, hot readers: %d\n", edges, len(f.readers))
	fmt.Fprintf(w, "reads answered with posts: %d served posts, %d beyond the first page, %d of %d home reads non-empty\n", served, deep, nonEmpty, home)
	return nil
}
