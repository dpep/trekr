class InviteJob
  def perform(user)
    InviteMailer.send_instructions(user)
  end
end
