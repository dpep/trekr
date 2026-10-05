RSpec.shared_context "with a widget client" do
  it "times out" do
    expect(client_timeout_class).to be
    expect(library_name).to be
  end

  include_context "widget callbacks"
end

RSpec.shared_context "widget callbacks" do
  it "skips its own library" do
    expect(library_tag).to be
  end
end
