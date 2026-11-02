RSpec.describe CleanupJob do
  it "cleans up" do
    CleanupJob.new.perform
  end
end
