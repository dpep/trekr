RSpec.describe "events" do
  let(:events_page) { PageObjects::EventsPage.new }

  it do
    expect(events_page).to have_event_count(1)
    expect(events_page).to have_no_banner
    expect(page).to be_visible
    expect(events_page).to be_exist
    expect(events_page).to be_an_admin
    expect(events_page).to be_within(1)
  end
end
