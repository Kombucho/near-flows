import { Route, Routes } from "react-router-dom";
import { Workspace } from "./ui/Workspace";

export function App() {
  return (
    <Routes>
      <Route path="*" element={<Workspace />} />
    </Routes>
  );
}
