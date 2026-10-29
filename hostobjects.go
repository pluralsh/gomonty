package monty

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"strings"
)

// hostObjects maps instance IDs of host dataclasses with methods to their
// values for one dispatch loop, so Monty's method calls (which carry only the
// receiver's instance ID) can reach Dataclass.Methods.
type hostObjects struct {
	byID map[string]Dataclass
}

func newHostObjects() *hostObjects {
	return &hostObjects{byID: map[string]Dataclass{}}
}

func (h *hostObjects) lookup(id string) (Dataclass, bool) {
	if h == nil {
		return Dataclass{}, false
	}
	dataclass, ok := h.byID[id]
	return dataclass, ok
}

func (h *hostObjects) prepareMap(values map[string]Value) map[string]Value {
	if len(values) == 0 {
		return values
	}
	prepared := make(map[string]Value, len(values))
	for key, value := range values {
		prepared[key] = h.prepare(value)
	}
	return prepared
}

func (h *hostObjects) prepareResult(result Result) Result {
	if result.kind != resultKindReturn {
		return result
	}
	result.value = h.prepare(result.value)
	return result
}

// prepare assigns missing dataclass IDs inside value and registers every
// dataclass that has methods.
func (h *hostObjects) prepare(value Value) Value {
	prepared, _ := h.prepareValue(value)
	return prepared
}

func (h *hostObjects) prepareValue(value Value) (Value, bool) {
	switch value.Kind() {
	case valueKindDataclass:
		dataclass := value.data.(Dataclass)
		withIDs := dataclassWithIDs(dataclass)
		withIDs.Attrs, _ = h.prepareDict(dataclass.Attrs)
		if len(withIDs.Methods) > 0 {
			h.byID[withIDs.InstanceID] = withIDs
		}
		return Value{kind: valueKindDataclass, data: withIDs}, true
	case valueKindList:
		items, changed := h.prepareValues(value.data.([]Value))
		if !changed {
			return value, false
		}
		return Value{kind: valueKindList, data: items}, true
	case valueKindTuple:
		items, changed := h.prepareValues(value.data.(Tuple))
		if !changed {
			return value, false
		}
		return Value{kind: valueKindTuple, data: Tuple(items)}, true
	case valueKindSet:
		items, changed := h.prepareValues(value.data.(Set))
		if !changed {
			return value, false
		}
		return Value{kind: valueKindSet, data: Set(items)}, true
	case valueKindFrozenSet:
		items, changed := h.prepareValues(value.data.(FrozenSet))
		if !changed {
			return value, false
		}
		return Value{kind: valueKindFrozenSet, data: FrozenSet(items)}, true
	case valueKindNamedTuple:
		namedTuple := value.data.(NamedTuple)
		items, changed := h.prepareValues(namedTuple.Values)
		if !changed {
			return value, false
		}
		namedTuple.Values = items
		return Value{kind: valueKindNamedTuple, data: namedTuple}, true
	case valueKindDict:
		dict, changed := h.prepareDict(value.data.(Dict))
		if !changed {
			return value, false
		}
		return Value{kind: valueKindDict, data: dict}, true
	default:
		return value, false
	}
}

func (h *hostObjects) prepareValues(values []Value) ([]Value, bool) {
	var prepared []Value
	for i, value := range values {
		next, changed := h.prepareValue(value)
		if changed && prepared == nil {
			prepared = append(make([]Value, 0, len(values)), values[:i]...)
		}
		if prepared != nil {
			prepared = append(prepared, next)
		}
	}
	if prepared == nil {
		return values, false
	}
	return prepared, true
}

func (h *hostObjects) prepareDict(dict Dict) (Dict, bool) {
	var prepared Dict
	for i, pair := range dict {
		key, keyChanged := h.prepareValue(pair.Key)
		value, valueChanged := h.prepareValue(pair.Value)
		if (keyChanged || valueChanged) && prepared == nil {
			prepared = append(make(Dict, 0, len(dict)), dict[:i]...)
		}
		if prepared != nil {
			prepared = append(prepared, Pair{Key: key, Value: value})
		}
	}
	if prepared == nil {
		return dict, false
	}
	return prepared, true
}

// dataclassWithIDs fills in a missing ClassID (derived from Name and TypeID,
// so every value of one Go type shares a class) and InstanceID (random).
func dataclassWithIDs(dataclass Dataclass) Dataclass {
	if dataclass.ClassID == "" {
		dataclass.ClassID = derivedClassID(dataclass.Name, dataclass.TypeID)
	}
	if dataclass.InstanceID == "" {
		dataclass.InstanceID = newRandomID()
	}
	return dataclass
}

func derivedClassID(name string, typeID uint64) string {
	hash := sha256.New()
	hash.Write([]byte("gomonty-dataclass\x00"))
	hash.Write([]byte(name))
	var typeIDBytes [8]byte
	binary.BigEndian.PutUint64(typeIDBytes[:], typeID)
	hash.Write(typeIDBytes[:])
	var id [16]byte
	copy(id[:], hash.Sum(nil))
	id[6] = (id[6] & 0x0f) | 0x80 // version 8: custom
	id[8] = (id[8] & 0x3f) | 0x80 // RFC 9562 variant
	return formatUUID(id)
}

func newRandomID() string {
	var id [16]byte
	_, _ = rand.Read(id[:])
	id[6] = (id[6] & 0x0f) | 0x40 // version 4: random
	id[8] = (id[8] & 0x3f) | 0x80 // RFC 9562 variant
	return formatUUID(id)
}

func formatUUID(id [16]byte) string {
	encoded := hex.EncodeToString(id[:])
	return encoded[0:8] + "-" + encoded[8:12] + "-" + encoded[12:16] + "-" + encoded[16:20] + "-" + encoded[20:32]
}

func uuidFromBytes(bytes []byte) string {
	if len(bytes) != 16 {
		return ""
	}
	return formatUUID([16]byte(bytes))
}

func parseUUID(value string) ([]byte, error) {
	decoded, err := hex.DecodeString(strings.ReplaceAll(value, "-", ""))
	if err != nil || len(decoded) != 16 {
		return nil, fmt.Errorf("invalid UUID %q", value)
	}
	return decoded, nil
}
