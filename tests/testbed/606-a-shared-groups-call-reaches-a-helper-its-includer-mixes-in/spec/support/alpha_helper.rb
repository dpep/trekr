module AlphaClientHelper
  def client_timeout_class
    Timeout::Error
  end

  def library_name
    :alpha
  end

  def never_called
    :alpha
  end

  def library_tag
    :alpha
  end
end
