RSpec.describe Widget do
  with_widget

  it { sign_in(1) }
end
