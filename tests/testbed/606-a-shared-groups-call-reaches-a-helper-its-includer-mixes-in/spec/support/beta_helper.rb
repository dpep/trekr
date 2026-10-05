module BetaClientHelper
  def client_timeout_class
    Timeout::Error
  end

  def library_name
    :beta
  end

  def library_tag
    :beta
  end
end
