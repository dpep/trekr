class Widget
  validate :check_it

  def check_it
  end

  def used_once
  end

  def never_used
  end

  def run
    used_once
  end
end
