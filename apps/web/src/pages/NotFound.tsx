import Button from "../components/ui/Button";
import EmptyState from "../components/ui/EmptyState";
import PageHeader from "../components/ui/PageHeader";
import { usePageTitle } from "../hooks/usePageTitle";

export default function NotFound(): React.JSX.Element {
  usePageTitle("Not found");

  return (
    <section>
      <PageHeader
        title="Not found"
        description="That page does not exist, or it was moved."
      />
      <EmptyState
        title="Nothing here"
        description="Check the address, or head back to the dashboard to find your servers."
        action={<Button to="/">Back to dashboard</Button>}
      />
    </section>
  );
}
