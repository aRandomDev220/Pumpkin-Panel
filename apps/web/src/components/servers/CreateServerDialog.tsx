import { useState } from "react";
import type { FormEvent } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import Button from "../ui/Button";
import Dialog from "../ui/Dialog";
import Field, { Input, Select } from "../ui/Field";
import { useToast } from "../ui/toast-context";
import { humanizeError } from "../../lib/errors";
import {
  createServer,
  digestWarning,
  validateCreateServer,
} from "../../lib/servers";

const RECOMMENDED_IMAGE = "ghcr.io/pumpkin-mc/pumpkin:master";
const RECOMMENDED_DIGEST =
  "sha256:675adcf4f351ed8656385346bf514584b0bb34826f06b553cd16094196d79099";

interface FormErrors {
  name?: string;
  port?: string;
  image?: string;
}

export interface CreateServerDialogProps {
  open: boolean;
  onClose: () => void;
}

export default function CreateServerDialog({
  open,
  onClose,
}: CreateServerDialogProps): React.JSX.Element {
  const queryClient = useQueryClient();
  const toast = useToast();
  const [name, setName] = useState("");
  const [gamePort, setGamePort] = useState("25565");
  const [customImage, setCustomImage] = useState(false);
  const [imageReference, setImageReference] = useState(RECOMMENDED_IMAGE);
  const [imageDigest, setImageDigest] = useState(RECOMMENDED_DIGEST);
  const [errors, setErrors] = useState<FormErrors>({});
  const [formError, setFormError] = useState<string | null>(null);

  const mutation = useMutation({
    mutationFn: createServer,
    onSuccess: (_created, input) => {
      void queryClient.invalidateQueries({ queryKey: ["servers"] });
      void queryClient.invalidateQueries({ queryKey: ["metrics", "summary"] });
      void queryClient.invalidateQueries({ queryKey: ["alerts"] });
      toast.success(`Server “${input.name}” created`);
      reset();
      onClose();
    },
    onError: (error) => setFormError(humanizeError(error)),
  });

  function reset(): void {
    setName("");
    setGamePort("25565");
    setCustomImage(false);
    setImageReference(RECOMMENDED_IMAGE);
    setImageDigest(RECOMMENDED_DIGEST);
    setErrors({});
    setFormError(null);
  }

  function handleClose(): void {
    if (mutation.isPending) return;
    reset();
    onClose();
  }

  function validate(): boolean {
    const next: FormErrors = {};
    const trimmed = name.trim();
    if (trimmed === "") {
      next.name = "Enter a server name.";
    } else if (!/^[a-zA-Z0-9][a-zA-Z0-9_-]*$/.test(trimmed)) {
      next.name =
        "Start with a letter or number; use letters, numbers, - and _ only.";
    }

    const port = Number(gamePort);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      next.port = "Use a port between 1 and 65535.";
    }

    if (customImage && imageReference.trim() === "") {
      next.image = "Enter an image reference.";
    }

    setErrors(next);
    return Object.keys(next).length === 0;
  }

  function onSubmit(event: FormEvent): void {
    event.preventDefault();
    setFormError(null);
    if (!validate()) return;

    const image = customImage
      ? imageReference.trim()
      : RECOMMENDED_IMAGE;
    const digest = customImage
      ? imageDigest.trim()
      : RECOMMENDED_DIGEST;
    const validation = validateCreateServer({
      name: name.trim(),
      game_port: Number(gamePort),
      image_reference: image,
    });
    if (validation !== null) {
      setFormError(validation);
      return;
    }

    mutation.mutate({
      name: name.trim(),
      game_port: Number(gamePort),
      image_reference: image,
      image_digest: digest === "" ? undefined : digest,
    });
  }

  const pinnedDigest = imageDigest.trim();
  const pinnedReference =
    pinnedDigest === ""
      ? imageReference
      : `${imageReference}${pinnedDigest.startsWith("@") ? "" : "@"}${pinnedDigest}`;
  const warning = customImage ? digestWarning(pinnedReference) : null;

  return (
    <Dialog
      open={open}
      onClose={handleClose}
      title="New server"
      description="Give the server a name and a port. It starts stopped, ready for its first run."
    >
      <form onSubmit={onSubmit} noValidate>
        <Field
          label="Name"
          error={errors.name}
          hint="Letters, numbers, - and _."
        >
          <Input
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="survival-1"
            required
            autoFocus
          />
        </Field>

        <Field
          label="Game port"
          error={errors.port}
          hint="Java players connect on this port."
        >
          <Input
            value={gamePort}
            onChange={(event) => setGamePort(event.target.value)}
            inputMode="numeric"
            placeholder="25565"
            required
          />
        </Field>

        <Field
          label="Server version"
          hint="The recommended build is pinned and tested."
        >
          <Select
            value={customImage ? "custom" : "recommended"}
            onChange={(event) =>
              setCustomImage(event.target.value === "custom")
            }
          >
            <option value="recommended">Recommended Pumpkin build</option>
            <option value="custom">Custom image (advanced)</option>
          </Select>
        </Field>

        {customImage && (
          <>
            <Field
              label="Image reference"
              error={errors.image}
              hint="Only allowlisted Pumpkin images are accepted."
            >
              <Input
                value={imageReference}
                onChange={(event) => setImageReference(event.target.value)}
                placeholder="ghcr.io/pumpkin-mc/pumpkin:tag"
              />
            </Field>
            <Field
              label="Digest (optional)"
              hint="Pin a digest for reproducible updates."
            >
              <Input
                value={imageDigest}
                onChange={(event) => setImageDigest(event.target.value)}
                placeholder="sha256:…"
              />
            </Field>
          </>
        )}

        {warning !== null && (
          <p className="warn" role="note">
            {warning}
          </p>
        )}
        {formError !== null && (
          <p className="err" role="alert">
            {formError}
          </p>
        )}

        <div className="dialog-actions">
          <Button
            variant="secondary"
            type="button"
            onClick={handleClose}
            disabled={mutation.isPending}
          >
            Cancel
          </Button>
          <Button type="submit" loading={mutation.isPending}>
            Create server
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
