class Account
  def email; end
end

class Profile
  belongs_to :account
  delegate_missing_to :account

  def label
    email
  end
end

class Presenter
  delegate_missing_to :account
  attr_reader :account
end

class Job
  def run
    profile = Profile.new
    profile.email
    profile.nope
    Presenter.new.email
  end
end
