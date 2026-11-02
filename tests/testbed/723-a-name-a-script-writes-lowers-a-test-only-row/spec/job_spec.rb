RSpec.describe Job do
  it "works" do
    job = Job.new
    job.expand_path
    job.expand_path
    job.restart
    job.restart
  end
end
