RSpec.describe Widget do
  it "works" do
    Widget::Api.new.fetch
    Widget.configure
    Widget.helper
    Widget::Version.new.major
    Widget::Cache::Entry.new.expire
  end
end
